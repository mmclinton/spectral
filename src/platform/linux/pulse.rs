use crate::state::lock_or_recover;
use libpulse_binding as pulse;
use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::{Context, FlagSet as ContextFlagSet};
use libpulse_binding::mainloop::threaded::Mainloop;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, PoisonError};

pub struct PulseContext {
    mainloop: Mainloop,
    context: Rc<RefCell<Context>>,
}

impl PulseContext {
    pub fn new(app_name: &str) -> Result<Self, String> {
        let mut mainloop =
            Mainloop::new().ok_or_else(|| "Failed to create mainloop".to_string())?;

        let context = Context::new(&mainloop, app_name)
            .ok_or_else(|| "Failed to create context".to_string())?;

        let context = Rc::new(RefCell::new(context));

        // Must start mainloop before locking
        mainloop
            .start()
            .map_err(|e| format!("Failed to start mainloop: {}", e))?;

        mainloop.lock();

        {
            let mut ctx = context.borrow_mut();
            ctx.connect(None, ContextFlagSet::NOFLAGS, None)
                .map_err(|e| {
                    mainloop.unlock();
                    format!("Failed to connect: {}", e)
                })?;
        }

        mainloop.unlock();

        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(5);

        loop {
            let state = {
                mainloop.lock();
                let s = context.borrow().get_state();
                mainloop.unlock();
                s
            };

            match state {
                pulse::context::State::Ready => break,
                pulse::context::State::Failed | pulse::context::State::Terminated => {
                    return Err(format!("Connection failed, state: {:?}", state));
                }
                _ => {
                    if start.elapsed() > timeout {
                        return Err("Timeout waiting for PulseAudio connection".to_string());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }

        Ok(PulseContext { mainloop, context })
    }

    /// Run a PulseAudio operation under the mainloop lock and block until its
    /// completion callback fires, with a 5 s timeout. `register` receives the
    /// shared result slot and must invoke the operation with a callback that
    /// stores `Some(value)` and notifies. The three module operations differ
    /// only in the call and the post-validation each does on the returned value.
    fn wait_for_callback<T: Send + 'static>(
        &mut self,
        timeout_label: &str,
        register: impl FnOnce(Arc<(Mutex<Option<T>>, Condvar)>),
    ) -> Result<T, String> {
        let result = Arc::new((Mutex::new(None), Condvar::new()));

        self.mainloop.lock();
        register(result.clone());
        self.mainloop.unlock();

        let (lock, cvar) = &*result;
        let timeout = std::time::Duration::from_secs(5);
        let (mut slot, wait) = cvar
            .wait_timeout_while(lock_or_recover(lock), timeout, |slot| slot.is_none())
            .unwrap_or_else(PoisonError::into_inner);

        if wait.timed_out() {
            return Err(format!("Timeout {timeout_label}"));
        }

        Ok(slot
            .take()
            .expect("PulseAudio signaled completion without a result"))
    }

    pub fn load_module(&mut self, name: &str, argument: &str) -> Result<u32, String> {
        let context = self.context.clone();
        let index = self.wait_for_callback("loading module", move |result| {
            context
                .borrow()
                .introspect()
                .load_module(name, argument, move |index| {
                    let (lock, cvar) = &*result;
                    *lock_or_recover(lock) = Some(index);
                    cvar.notify_one();
                });
        })?;

        if index == pulse::def::INVALID_INDEX {
            return Err("Failed to load module (invalid index returned)".to_string());
        }

        Ok(index)
    }

    pub fn unload_module(&mut self, index: u32) -> Result<(), String> {
        let context = self.context.clone();
        let success = self.wait_for_callback("unloading module", move |result| {
            context
                .borrow_mut()
                .introspect()
                .unload_module(index, move |success| {
                    let (lock, cvar) = &*result;
                    *lock_or_recover(lock) = Some(success);
                    cvar.notify_one();
                });
        })?;

        if !success {
            return Err("Failed to unload module".to_string());
        }

        Ok(())
    }

    /// Run an introspection list operation under the mainloop lock and collect
    /// the items the per-item callback pushes, blocking until end-of-list with a
    /// 5 s timeout. `register` invokes the list call with a callback that pushes
    /// onto `state.0` and signals `state.1` on `End`/`Error`.
    fn collect_list<T: Send + 'static>(
        &mut self,
        timeout_label: &str,
        register: impl FnOnce(Arc<(Mutex<ListAccum<T>>, Condvar)>),
    ) -> Result<Vec<T>, String> {
        let state = Arc::new((Mutex::new(ListAccum::<T>::new()), Condvar::new()));

        self.mainloop.lock();
        register(state.clone());
        self.mainloop.unlock();

        let (lock, cvar) = &*state;
        let timeout = std::time::Duration::from_secs(5);
        let (mut guard, wait) = cvar
            .wait_timeout_while(lock_or_recover(lock), timeout, |accum| !accum.finished)
            .unwrap_or_else(PoisonError::into_inner);

        if wait.timed_out() {
            return Err(format!("Timeout {timeout_label}"));
        }
        if guard.error {
            return Err(format!("Error {timeout_label}"));
        }

        Ok(std::mem::take(&mut guard.items))
    }

    /// Poll until a source with the given name is visible in the server.
    /// A module load completing does not guarantee its source node is
    /// registered yet; callers that immediately address the source by name
    /// need this positive check.
    pub fn wait_for_source(&mut self, name: &str) -> Result<(), String> {
        const SOURCE_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
        const SOURCE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

        let deadline = std::time::Instant::now() + SOURCE_READY_TIMEOUT;
        loop {
            let context = self.context.clone();
            let name_owned = name.to_string();
            let found = self.collect_list("querying source", move |state| {
                context
                    .borrow()
                    .introspect()
                    .get_source_info_by_name(&name_owned, move |result| match result {
                        ListResult::Item(_) => lock_or_recover(&state.0).items.push(()),
                        ListResult::End => {
                            lock_or_recover(&state.0).finished = true;
                            state.1.notify_one();
                        }
                        ListResult::Error => {
                            let mut accum = lock_or_recover(&state.0);
                            accum.error = true;
                            accum.finished = true;
                            state.1.notify_one();
                        }
                    });
            });

            // A source that does not exist yet reports ListResult::Error, so
            // within the deadline both Err and an empty list mean "not yet".
            match found {
                Ok(items) if !items.is_empty() => return Ok(()),
                _ if std::time::Instant::now() >= deadline => {
                    return Err(format!(
                        "source {name} did not appear within {SOURCE_READY_TIMEOUT:?}"
                    ));
                }
                _ => std::thread::sleep(SOURCE_POLL_INTERVAL),
            }
        }
    }

    /// Unload Spectral virtual-mic modules left loaded by a previous daemon
    /// instance (a crash or SIGKILL bypasses Drop, leaving the server-side
    /// modules behind). Sinks are matched by their constant `device.description`;
    /// the paired remap source by name (the sink name without its `_sink`
    /// suffix). Returns the number of modules unloaded.
    pub fn reap_spectral_modules(&mut self) -> Result<usize, String> {
        const SINK_DESCRIPTION: &str = "Spectral-DoNotUse";
        const SINK_SUFFIX: &str = "_sink";

        let context = self.context.clone();
        let sinks: Vec<(Option<u32>, String)> =
            self.collect_list("listing sinks", move |state| {
                context
                    .borrow()
                    .introspect()
                    .get_sink_info_list(move |result| match result {
                        ListResult::Item(info) => {
                            if info.proplist.get_str("device.description").as_deref()
                                == Some(SINK_DESCRIPTION)
                            {
                                let name = info
                                    .name
                                    .as_ref()
                                    .map(|n| n.to_string())
                                    .unwrap_or_default();
                                lock_or_recover(&state.0)
                                    .items
                                    .push((info.owner_module, name));
                            }
                        }
                        ListResult::End => {
                            lock_or_recover(&state.0).finished = true;
                            state.1.notify_one();
                        }
                        ListResult::Error => {
                            let mut accum = lock_or_recover(&state.0);
                            accum.error = true;
                            accum.finished = true;
                            state.1.notify_one();
                        }
                    });
            })?;

        let source_names: std::collections::HashSet<String> = sinks
            .iter()
            .filter_map(|(_, name)| name.strip_suffix(SINK_SUFFIX).map(str::to_string))
            .collect();

        let mut modules: Vec<u32> = sinks.into_iter().filter_map(|(module, _)| module).collect();

        if !source_names.is_empty() {
            let context = self.context.clone();
            let sources: Vec<u32> = self.collect_list("listing sources", move |state| {
                context
                    .borrow()
                    .introspect()
                    .get_source_info_list(move |result| match result {
                        ListResult::Item(info) => {
                            let name = info
                                .name
                                .as_ref()
                                .map(|n| n.to_string())
                                .unwrap_or_default();
                            if source_names.contains(&name)
                                && let Some(module) = info.owner_module
                            {
                                lock_or_recover(&state.0).items.push(module);
                            }
                        }
                        ListResult::End => {
                            lock_or_recover(&state.0).finished = true;
                            state.1.notify_one();
                        }
                        ListResult::Error => {
                            let mut accum = lock_or_recover(&state.0);
                            accum.error = true;
                            accum.finished = true;
                            state.1.notify_one();
                        }
                    });
            })?;
            modules.extend(sources);
        }

        modules.sort_unstable();
        modules.dedup();

        let mut count = 0;
        for index in modules {
            match self.unload_module(index) {
                Ok(()) => count += 1,
                Err(e) => eprintln!("[reap] failed to unload module {index}: {e}"),
            }
        }

        Ok(count)
    }
}

impl Drop for PulseContext {
    fn drop(&mut self) {
        self.mainloop.lock();
        self.context.borrow_mut().disconnect();
        self.mainloop.unlock();
        self.mainloop.stop();
    }
}

struct ListAccum<T> {
    items: Vec<T>,
    finished: bool,
    error: bool,
}

impl<T> ListAccum<T> {
    fn new() -> Self {
        Self {
            items: Vec::new(),
            finished: false,
            error: false,
        }
    }
}

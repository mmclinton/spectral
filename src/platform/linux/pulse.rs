use crate::state::lock_or_recover;
use libpulse_binding as pulse;
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
                pulse::context::State::Ready => {
                    println!("[PulseAudio] Connected successfully");
                    break;
                }
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

    pub fn set_default_source(&mut self, source_name: &str) -> Result<(), String> {
        let context = self.context.clone();
        let success = self.wait_for_callback("setting default source", move |result| {
            context
                .borrow_mut()
                .set_default_source(source_name, move |success| {
                    let (lock, cvar) = &*result;
                    *lock_or_recover(lock) = Some(success);
                    cvar.notify_one();
                });
        })?;

        if !success {
            return Err("Failed to set default source".to_string());
        }

        Ok(())
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
}

impl Drop for PulseContext {
    fn drop(&mut self) {
        self.mainloop.lock();
        self.context.borrow_mut().disconnect();
        self.mainloop.unlock();
        self.mainloop.stop();
    }
}

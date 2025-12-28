// PulseAudio module management using libpulse-binding
// This replaces pactl shell commands with native Rust API calls

use libpulse_binding as pulse;
use libpulse_binding::context::{Context, FlagSet as ContextFlagSet};
use libpulse_binding::mainloop::threaded::Mainloop;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex};

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

    pub fn load_module(&mut self, name: &str, argument: &str) -> Result<u32, String> {
        let result = Arc::new((Mutex::new(None), Condvar::new()));
        let result_clone = result.clone();

        self.mainloop.lock();

        {
            let mut introspect = self.context.borrow().introspect();
            introspect.load_module(name, argument, move |index| {
                let (lock, cvar) = &*result_clone;
                let mut result = lock.lock().unwrap();
                *result = Some(index);
                cvar.notify_one();
            });
        }

        self.mainloop.unlock();

        let (lock, cvar) = &*result;
        let timeout = std::time::Duration::from_secs(5);
        let wait_result = cvar
            .wait_timeout_while(lock.lock().unwrap(), timeout, |result| result.is_none())
            .unwrap();

        if wait_result.1.timed_out() {
            return Err("Timeout loading module".to_string());
        }

        let index = wait_result.0.unwrap();
        if index == pulse::def::INVALID_INDEX {
            return Err("Failed to load module (invalid index returned)".to_string());
        }

        Ok(index)
    }

    pub fn set_default_source(&mut self, source_name: &str) -> Result<(), String> {
        let result = Arc::new((Mutex::new(None), Condvar::new()));
        let result_clone = result.clone();

        self.mainloop.lock();

        self.context
            .borrow_mut()
            .set_default_source(source_name, move |success| {
                let (lock, cvar) = &*result_clone;
                let mut result = lock.lock().unwrap();
                *result = Some(success);
                cvar.notify_one();
            });

        self.mainloop.unlock();

        let (lock, cvar) = &*result;
        let timeout = std::time::Duration::from_secs(5);
        let wait_result = cvar
            .wait_timeout_while(lock.lock().unwrap(), timeout, |result| result.is_none())
            .unwrap();

        if wait_result.1.timed_out() {
            return Err("Timeout setting default source".to_string());
        }

        let success = wait_result.0.unwrap();
        if !success {
            return Err("Failed to set default source".to_string());
        }

        Ok(())
    }

    pub fn unload_module(&mut self, index: u32) -> Result<(), String> {
        let result = Arc::new((Mutex::new(None), Condvar::new()));
        let result_clone = result.clone();

        self.mainloop.lock();

        {
            let mut introspect = self.context.borrow_mut().introspect();
            introspect.unload_module(index, move |success| {
                let (lock, cvar) = &*result_clone;
                let mut result = lock.lock().unwrap();
                *result = Some(success);
                cvar.notify_one();
            });
        }

        self.mainloop.unlock();

        let (lock, cvar) = &*result;
        let timeout = std::time::Duration::from_secs(5);
        let wait_result = cvar
            .wait_timeout_while(lock.lock().unwrap(), timeout, |result| result.is_none())
            .unwrap();

        if wait_result.1.timed_out() {
            return Err("Timeout unloading module".to_string());
        }

        let success = wait_result.0.unwrap();
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

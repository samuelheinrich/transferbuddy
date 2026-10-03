//! One desktop per configuration directory. TUI processes remain independent.
use crate::{app::UiEvent, native::NativeAction};
use fs2::FileExt;
#[cfg(unix)]
use std::os::unix::{
    fs::PermissionsExt,
    net::{UnixListener, UnixStream},
};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};
pub struct Instance {
    _lock: File,
    stop: Arc<AtomicBool>,
    socket: std::path::PathBuf,
}
impl Instance {
    pub fn acquire(
        dir: &std::path::Path,
        ctx: eframe::egui::Context,
        tx: mpsc::Sender<UiEvent>,
    ) -> anyhow::Result<Option<Self>> {
        let dir = dir.join("desktop-state");
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join("instance.lock"))?;
        // macOS sockaddr_un has a small path limit; hash the canonical config directory.
        let key = std::collections::hash_map::DefaultHasher::new();
        use std::hash::{Hash, Hasher};
        let mut key = key;
        dir.canonicalize()?.hash(&mut key);
        let socket =
            std::path::PathBuf::from("/tmp").join(format!("tb-desktop-{:016x}.sock", key.finish()));
        if let Err(e) = lock.try_lock_exclusive() {
            if e.kind() != std::io::ErrorKind::WouldBlock {
                return Err(e.into());
            }
            #[cfg(unix)]
            {
                let mut stream = None;
                for _ in 0..20 {
                    match UnixStream::connect(&socket) {
                        Ok(s) => {
                            stream = Some(s);
                            break;
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(50)),
                    }
                }
                let mut stream = stream
                    .ok_or_else(|| anyhow::anyhow!("Desktop is running but cannot be reached"))?;
                stream.write_all(b"show")?;
            }
            return Ok(None);
        }
        #[cfg(unix)]
        {
            if socket.exists() {
                std::fs::remove_file(&socket)?;
            }
            let listener = UnixListener::bind(&socket)?;
            std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
            listener.set_nonblocking(true)?;
            let stop = Arc::new(AtomicBool::new(false));
            let stopped = stop.clone();
            std::thread::spawn(move || {
                while !stopped.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                            let mut message = [0; 4];
                            if stream.read_exact(&mut message).is_ok() && message == *b"show" {
                                let _ = tx.send(UiEvent::Native(NativeAction::Show));
                                ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Visible(true));
                                ctx.request_repaint();
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(50))
                        }
                        Err(_) => break,
                    }
                }
            });
            Ok(Some(Self {
                _lock: lock,
                stop,
                socket,
            }))
        }
        #[cfg(not(unix))]
        {
            let _ = (ctx, tx);
            Ok(Some(Self {
                _lock: lock,
                stop: Arc::new(AtomicBool::new(false)),
                socket,
            }))
        }
    }
}
impl Drop for Instance {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = std::fs::remove_file(&self.socket);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn second_launch_reopens_and_release_allows_a_new_instance() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, tx) = (eframe::egui::Context::default(), mpsc::channel());
        let (sender, receiver) = tx;
        let first = Instance::acquire(dir.path(), ctx.clone(), sender.clone())
            .unwrap()
            .unwrap();
        assert!(Instance::acquire(dir.path(), ctx.clone(), sender.clone())
            .unwrap()
            .is_none());
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
            UiEvent::Native(NativeAction::Show)
        ));
        drop(first);
        assert!(Instance::acquire(dir.path(), ctx, sender)
            .unwrap()
            .is_some());
    }
    #[test]
    fn different_configurations_run_independently() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let (tx, _rx) = mpsc::channel();
        let ctx = eframe::egui::Context::default();
        let _a = Instance::acquire(first.path(), ctx.clone(), tx.clone())
            .unwrap()
            .unwrap();
        let _b = Instance::acquire(second.path(), ctx, tx).unwrap().unwrap();
    }
}

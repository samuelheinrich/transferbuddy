use crate::app::UiEvent;
use eframe::egui;
use muda::{Menu, MenuEvent, MenuItem, Submenu};
use std::sync::mpsc;
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
#[derive(Clone, Copy, Debug)]
pub enum NativeAction {
    Show,
    Quit,
    Folder,
    Settings,
    StopAll,
    About,
    Help,
    Copy,
    Cut,
    Paste,
    SelectAll,
    Command(crate::app::Intent),
}
pub struct NativeShell {
    _menu: Menu,
    tray: TrayIcon,
    last: Option<(usize, bool)>,
    commands: Vec<(MenuItem, crate::app::Intent, Option<bool>)>,
}
impl NativeShell {
    pub fn new(ctx: egui::Context, tx: mpsc::Sender<UiEvent>) -> anyhow::Result<Self> {
        #[cfg(target_os = "macos")]
        let _main = objc2::MainThreadMarker::new()
            .ok_or_else(|| anyhow::anyhow!("Native menu needs the main thread"))?;
        let menu = Menu::new();
        let sub = Submenu::new("TransferBuddy", true);
        let tray_menu = Menu::new();
        let mut actions = Vec::new();
        let mut commands = Vec::new();
        for (label, action) in [
            ("Show TransferBuddy", NativeAction::Show),
            ("Choose folder…", NativeAction::Folder),
            ("Settings…", NativeAction::Settings),
            ("Stop all services", NativeAction::StopAll),
            ("About TransferBuddy", NativeAction::About),
            ("Quit TransferBuddy", NativeAction::Quit),
        ] {
            use muda::accelerator::{Accelerator, Code, Modifiers};
            let shortcut = match action {
                NativeAction::Quit => Some(Accelerator::new(Modifiers::META, Code::KeyQ)),
                NativeAction::Folder => Some(Accelerator::new(Modifiers::META, Code::KeyO)),
                NativeAction::Settings => Some(Accelerator::new(Modifiers::META, Code::Comma)),
                _ => None,
            };
            let item = MenuItem::new(label, true, shortcut);
            actions.push((item.id().clone(), action));
            sub.append(&item)?;
            tray_menu.append(&item)?;
        }
        menu.append(&sub)?;
        use crate::app::{Intent, Tab};
        use muda::accelerator::{Accelerator, Code, Modifiers};
        for (title, entries) in Intent::groups() {
            let submenu = Submenu::new(title, true);
            for intent in entries {
                let code = match intent {
                    Intent::Add(false) | Intent::Workflow => Some(Code::KeyN),
                    Intent::Jobs => Some(Code::KeyJ),
                    Intent::Refresh => Some(Code::KeyR),
                    Intent::View(tab) => Some(match tab {
                        Tab::Dashboard => Code::Digit1,
                        Tab::Connect => Code::Digit2,
                        Tab::Transfer => Code::Digit3,
                        Tab::Upgrade => Code::Digit4,
                        Tab::Logs => Code::Digit5,
                    }),
                    _ => None,
                };
                let label = intent.title();
                let item = MenuItem::new(
                    label,
                    true,
                    code.map(|code| {
                        Accelerator::new(
                            if matches!(intent, Intent::Workflow) {
                                Modifiers::META | Modifiers::SHIFT
                            } else {
                                Modifiers::META
                            },
                            code,
                        )
                    }),
                );
                actions.push((item.id().clone(), NativeAction::Command(intent)));
                commands.push((item.clone(), intent, None));
                submenu.append(&item)?;
            }
            menu.append(&submenu)?;
        }
        let edit = Submenu::new("Edit", true);
        edit.append(&muda::PredefinedMenuItem::undo(None))?;
        edit.append(&muda::PredefinedMenuItem::redo(None))?;
        edit.append(&muda::PredefinedMenuItem::separator())?;
        for (label, action, code) in [
            ("Cut", NativeAction::Cut, Code::KeyX),
            ("Copy", NativeAction::Copy, Code::KeyC),
            ("Paste", NativeAction::Paste, Code::KeyV),
            ("Select All", NativeAction::SelectAll, Code::KeyA),
        ] {
            let item = MenuItem::new(label, true, Some(Accelerator::new(Modifiers::META, code)));
            actions.push((item.id().clone(), action));
            edit.append(&item)?;
        }
        menu.append(&edit)?;
        let window = Submenu::new("Window", true);
        window.append(&muda::PredefinedMenuItem::minimize(None))?;
        window.append(&muda::PredefinedMenuItem::maximize(None))?;
        menu.append(&window)?;
        let help = Submenu::new("Help", true);
        let about = MenuItem::new("About TransferBuddy", true, None);
        actions.push((about.id().clone(), NativeAction::About));
        help.append(&about)?;
        let guide = MenuItem::new("TransferBuddy Help", true, None);
        actions.push((guide.id().clone(), NativeAction::Help));
        help.append(&guide)?;
        menu.append(&help)?;
        #[cfg(target_os = "macos")]
        menu.init_for_nsapp();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some((_, action)) = actions.iter().find(|(id, _)| *id == event.id) {
                let _ = tx.send(UiEvent::Native(*action));
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.request_repaint();
            }
        }));
        let rgba = crate::design::icon_rgba(32);
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(tray_menu))
            .with_tooltip(format!("TransferBuddy {}", transferbuddy_core::VERSION))
            .with_icon(Icon::from_rgba(rgba, 32, 32)?)
            .build()?;
        Ok(Self {
            _menu: menu,
            tray,
            last: None,
            commands,
        })
    }
    pub fn update_commands(&mut self, allowed: impl Fn(crate::app::Intent) -> bool) {
        for (item, intent, previous) in &mut self.commands {
            let enabled = allowed(*intent);
            if *previous != Some(enabled) {
                item.set_enabled(enabled);
                *previous = Some(enabled);
            }
        }
    }
    pub fn update(&mut self, questions: usize, active: bool) {
        if self.last == Some((questions, active)) {
            return;
        }
        self.last = Some((questions, active));
        let status = if questions > 0 {
            format!("{questions} confirmation(s) waiting")
        } else if active {
            "Jobs running".into()
        } else {
            "Ready".into()
        };
        let _ = self.tray.set_tooltip(Some(format!(
            "TransferBuddy {} — {status}",
            transferbuddy_core::VERSION
        )));
        #[cfg(target_os = "macos")]
        self.tray
            .set_title(if questions > 0 { Some("!") } else { None });
    }
}
#[derive(Default)]
pub struct Activity {
    #[cfg(target_os = "macos")]
    token: Option<
        objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_foundation::NSObjectProtocol>>,
    >,
}
impl Activity {
    pub fn set_active(&mut self, active: bool) {
        #[cfg(target_os = "macos")]
        {
            use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
            let info = NSProcessInfo::processInfo();
            if active && self.token.is_none() {
                self.token = Some(info.beginActivityWithOptions_reason(
                    NSActivityOptions::UserInitiated,
                    &NSString::from_str("TransferBuddy file transfer or device upgrade"),
                ));
            } else if !active {
                if let Some(token) = self.token.take() {
                    unsafe { info.endActivity(&token) }
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = active;
    }
}
impl Drop for Activity {
    fn drop(&mut self) {
        self.set_active(false)
    }
}
pub fn install_port_broker() {
    #[cfg(target_os = "macos")]
    transferbuddy_core::platform::install_broker(std::sync::Arc::new(
        transferbuddy_core::platform::MacBroker,
    ));
}
#[cfg(target_os = "macos")]
fn helper() -> Result<objc2::rc::Retained<objc2_service_management::SMAppService>, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let contents = exe
        .parent()
        .and_then(|p| p.parent())
        .ok_or("Cannot find application bundle")?;
    if !contents
        .join("Library/LaunchDaemons/com.transferbuddy.port-helper.plist")
        .is_file()
    {
        return Err("Install the packaged TransferBuddy.app first; helper registration requires its embedded launch daemon.".into());
    }
    Ok(unsafe {
        objc2_service_management::SMAppService::daemonServiceWithPlistName(
            &objc2_foundation::NSString::from_str("com.transferbuddy.port-helper.plist"),
        )
    })
}
pub fn enable_helper() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        let service = helper()?;
        unsafe { service.registerAndReturnError() }.map_err(|e| e.to_string())?;
        Ok("Helper registered. If macOS requests approval, enable TransferBuddy under System Settings → General → Login Items & Extensions.".into())
    }
    #[cfg(not(target_os = "macos"))]
    Err("The standard-port helper is available on macOS.".into())
}
pub fn disable_helper() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        unsafe { helper()?.unregisterAndReturnError() }.map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("The standard-port helper is available on macOS.".into())
    }
}

pub fn reduce_motion() -> bool {
    #[cfg(target_os = "macos")]
    {
        objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

pub fn open_network_privacy_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let result = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_LocalNetwork")
            .status()
            .map_err(|e| format!("Cannot open System Settings: {e}"))?;
        if result.success() {
            Ok(())
        } else {
            Err("Cannot open System Settings → Privacy & Security → Local Network".into())
        }
    }
    #[cfg(not(target_os = "macos"))]
    Err("Local Network privacy settings are available on macOS.".into())
}

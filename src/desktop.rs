//! macOS 菜单栏、开机启动和播放期间防休眠。

#[derive(Clone)]
pub struct SystemFont {
    pub family: String,
    pub label: String,
}

#[cfg(target_os = "macos")]
mod native {
    use std::cell::RefCell;
    use std::fs;
    use std::io::Write;
    use std::path::Path;
    use std::sync::OnceLock;

    use super::SystemFont;
    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringRef};
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{
        AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
    };
    use objc2_app_kit::{
        NSAlert, NSAlertFirstButtonReturn, NSApplication, NSEventMask, NSEventModifierFlags,
        NSEventType, NSFont, NSFontManager, NSFontTraitMask, NSImage, NSMenu, NSStatusBar,
        NSStatusBarButton, NSStatusItem, NSTextView, NSVariableStatusItemLength, NSView, NSWindow,
        NSWindowDidMiniaturizeNotification, NSWorkspace,
    };
    use objc2_foundation::{
        NSBundle, NSData, NSNotification, NSNotificationCenter, NSObject, NSSize, NSString, NSURL,
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    const LAUNCH_ID: &str = "com.yuanjie.tfybgm.gpui";

    pub fn reduce_motion() -> bool {
        NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
    }

    fn appkit_window(window: &gpui::Window) -> Option<Retained<NSWindow>> {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return None;
        };
        let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
        view.window()
    }

    pub fn configure_lyrics_window(window: &gpui::Window) {
        use objc2_app_kit::{
            NSStatusWindowLevel, NSWindowCollectionBehavior as Collection,
            NSWindowStyleMask as Mask,
        };

        if let Some(window) = appkit_window(window) {
            window.setStyleMask(Mask::Resizable | Mask::Closable | Mask::NonactivatingPanel);
            window.setLevel(NSStatusWindowLevel);
            window.setHasShadow(false);
            window.setHidesOnDeactivate(false);
            window.setMovableByWindowBackground(true);
            window.setCollectionBehavior(
                Collection::CanJoinAllSpaces
                    | Collection::FullScreenAuxiliary
                    | Collection::IgnoresCycle,
            );
            window.setContentMinSize(NSSize::new(420., 120.));
            window.setContentMaxSize(NSSize::new(1600., 900.));
            fit_lyrics_window(&window, true);
        }
    }

    pub fn fit_lyrics_bounds(window: &gpui::Window) {
        if let Some(window) = appkit_window(window) {
            fit_lyrics_window(&window, false);
        }
    }

    fn fit_lyrics_window(window: &NSWindow, place: bool) {
        let Some(screen) = window
            .screen()
            .or_else(|| MainThreadMarker::new().and_then(objc2_app_kit::NSScreen::mainScreen))
        else {
            return;
        };
        let area = screen.visibleFrame();
        let frame = window.frame();
        let width = (frame.size.width).min(area.size.width.max(420.));
        let height = (frame.size.height).min(area.size.height.max(120.));
        let x = if place {
            area.origin.x + (area.size.width - width) / 2.
        } else {
            frame.origin.x.clamp(
                area.origin.x,
                area.origin.x + (area.size.width - width).max(0.),
            )
        };
        let y = if place {
            area.origin.y + 32_f64.min((area.size.height - height).max(0.))
        } else {
            frame.origin.y.clamp(
                area.origin.y,
                area.origin.y + (area.size.height - height).max(0.),
            )
        };
        let next = objc2_foundation::NSRect::new(
            objc2_foundation::NSPoint::new(x, y),
            NSSize::new(width, height),
        );
        if frame != next {
            window.setFrame_display(next, true);
        }
    }

    pub fn lyrics_pointer(window: &gpui::Window, locked: bool) -> bool {
        use objc2_app_kit::{NSEvent, NSStatusWindowLevel};

        let Some(window) = appkit_window(window) else {
            return false;
        };
        let cursor = NSEvent::mouseLocation();
        let frame = window.frame();
        let x = cursor.x - frame.origin.x;
        let y = frame.size.height - (cursor.y - frame.origin.y);
        let hot =
            (frame.size.width - 48. ..=frame.size.width).contains(&x) && (0. ..=36.).contains(&y);
        let ignored = locked && !hot;
        if window.ignoresMouseEvents() != ignored {
            window.setIgnoresMouseEvents(ignored);
        }
        if window.isMovable() == locked {
            window.setMovable(!locked);
        }
        if window.level() != NSStatusWindowLevel {
            window.setLevel(NSStatusWindowLevel);
        }
        hot
    }

    pub fn anchor_lyrics_settings(settings: &gpui::Window, lyrics: &gpui::Window) {
        let (Some(settings), Some(lyrics)) = (appkit_window(settings), appkit_window(lyrics))
        else {
            return;
        };
        let Some(screen) = lyrics.screen() else {
            return;
        };
        let level = objc2_app_kit::NSStatusWindowLevel + 1;
        if settings.level() != level {
            settings.setLevel(level);
        }
        let visible = screen.visibleFrame();
        let overlay = lyrics.frame();
        let frame = settings.frame();
        let right = visible.origin.x + visible.size.width;
        let top = visible.origin.y + visible.size.height;
        let max_x = (right - frame.size.width).max(visible.origin.x);
        let max_y = (top - frame.size.height).max(visible.origin.y);
        let x = (overlay.origin.x + overlay.size.width - frame.size.width)
            .clamp(visible.origin.x, max_x);
        let above = overlay.origin.y + overlay.size.height + 8.;
        let below = overlay.origin.y - frame.size.height - 8.;
        let y = if above <= max_y {
            above
        } else {
            below.clamp(visible.origin.y, max_y)
        };
        let origin = objc2_foundation::NSPoint::new(x, y);
        if origin != frame.origin {
            settings.setFrameOrigin(origin);
        }
    }

    pub fn window_visible(window: &gpui::Window) -> bool {
        let Some(main_thread) = MainThreadMarker::new() else {
            return false;
        };
        !NSApplication::sharedApplication(main_thread).isHidden()
            && appkit_window(window)
                .is_some_and(|window| window.isVisible() && !window.isMiniaturized())
    }

    pub fn open_audio_file(path: &Path) -> Result<(), String> {
        let path = path.to_str().ok_or("无法识别音频文件路径")?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        NSWorkspace::sharedWorkspace()
            .openURL(&url)
            .then_some(())
            .ok_or_else(|| "macOS 无法打开该音频文件".into())
    }

    pub fn edit_subtitle_text(
        window: &gpui::Window,
        title: &str,
        raw: &str,
        error: Option<&str>,
    ) -> Result<tokio::sync::oneshot::Receiver<Option<String>>, String> {
        use block2::RcBlock;

        let main_thread = MainThreadMarker::new().ok_or("字幕编辑需要应用主线程")?;
        let window = appkit_window(window).ok_or("找不到字幕编辑的主窗口")?;
        let alert = NSAlert::new(main_thread);
        alert.setMessageText(&NSString::from_str(title));
        if let Some(error) = error {
            alert.setInformativeText(&NSString::from_str(error));
        }
        let scroll = NSTextView::scrollableTextView(main_thread);
        scroll.setFrameSize(NSSize::new(540., 290.));
        let text = scroll
            .documentView()
            .and_then(|view| view.downcast::<NSTextView>().ok())
            .ok_or("无法打开字幕文本编辑器")?;
        text.setRichText(false);
        text.setAllowsUndo(true);
        text.setAutomaticQuoteSubstitutionEnabled(false);
        text.setAutomaticDashSubstitutionEnabled(false);
        text.setAutomaticTextReplacementEnabled(false);
        text.setAutomaticSpellingCorrectionEnabled(false);
        let font = NSFont::fontWithName_size(&NSString::from_str("Menlo"), 13.);
        text.setFont(font.as_deref());
        text.setString(&NSString::from_str(raw));
        alert.setAccessoryView(Some(&scroll));
        let save = alert.addButtonWithTitle(&NSString::from_str("保存"));
        save.setKeyEquivalent(&NSString::from_str("s"));
        save.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
        alert.addButtonWithTitle(&NSString::from_str("取消"));
        let (sender, response) = tokio::sync::oneshot::channel();
        let sender = RefCell::new(Some(sender));
        let editor = text.clone();
        let complete = RcBlock::new(move |code: isize| {
            if let Some(sender) = sender.borrow_mut().take() {
                let _ = sender
                    .send((code == NSAlertFirstButtonReturn).then(|| editor.string().to_string()));
            }
        });
        alert.beginSheetModalForWindow_completionHandler(&window, Some(&complete));
        alert.window().makeFirstResponder(Some(&text));
        Ok(response)
    }

    pub fn system_fonts(_: &gpui::App) -> Vec<SystemFont> {
        static FONTS: OnceLock<Vec<SystemFont>> = OnceLock::new();
        let Some(main_thread) = MainThreadMarker::new() else {
            return Vec::new();
        };
        FONTS
            .get_or_init(|| {
                let manager = NSFontManager::sharedFontManager(main_thread);
                let mut families: Vec<_> = manager
                    .availableFontFamilies()
                    .iter()
                    .filter_map(|name| {
                        let family = name.to_string();
                        if family.starts_with(['.', '@']) || family.chars().count() > 96 {
                            return None;
                        }
                        let label = manager.localizedNameForFamily_face(&name, None).to_string();
                        let font = manager.fontWithFamily_traits_weight_size(
                            &name,
                            NSFontTraitMask::empty(),
                            5,
                            13.,
                        );
                        let cjk = font.is_some_and(|font| {
                            let characters = font.coveredCharacterSet();
                            ['中', '文']
                                .into_iter()
                                .all(|ch| characters.longCharacterIsMember(ch.into()))
                        });
                        let localized = label
                            .chars()
                            .any(|ch| matches!(ch, '\u{3400}'..='\u{9fff}'));
                        Some((!cjk, !localized, SystemFont { family, label }))
                    })
                    .collect();
                families.sort_by(|(a_cjk, a_localized, a), (b_cjk, b_localized, b)| {
                    (a_cjk, a_localized, &a.label).cmp(&(b_cjk, b_localized, &b.label))
                });
                families.into_iter().map(|(_, _, font)| font).collect()
            })
            .clone()
    }

    pub fn set_unbundled_icon() {
        if NSBundle::mainBundle().bundleIdentifier().is_some() {
            return;
        }
        let Some(main_thread) = MainThreadMarker::new() else {
            return;
        };
        let data = NSData::with_bytes(crate::assets::BRAND_MARK);
        if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
            // 裸可执行文件用品牌图，应用包由系统读取分层图标。
            unsafe {
                NSApplication::sharedApplication(main_thread).setApplicationIconImage(Some(&image))
            };
        }
    }

    struct TrayActions {
        restore: Box<dyn Fn()>,
        minimize: Box<dyn Fn()>,
        quit: Box<dyn Fn()>,
        window: Retained<NSWindow>,
        menu: Retained<NSMenu>,
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[ivars = TrayActions]
        struct TrayTarget;

        impl TrayTarget {
            #[unsafe(method(clicked:))]
            fn clicked(&self, button: &NSStatusBarButton) {
                let app = NSApplication::sharedApplication(self.mtm());
                if let Some(event) = app.currentEvent()
                    && event.r#type() == NSEventType::RightMouseDown
                {
                    NSMenu::popUpContextMenu_withEvent_forView(&self.ivars().menu, &event, button);
                } else {
                    self.restore_window();
                }
            }

            #[unsafe(method(restore:))]
            fn restore(&self, _sender: &AnyObject) {
                self.restore_window();
            }

            #[unsafe(method(minimized:))]
            fn minimized(&self, _notification: &NSNotification) {
                (self.ivars().minimize)();
            }

            #[unsafe(method(quit:))]
            fn quit(&self, _sender: &AnyObject) {
                (self.ivars().quit)();
            }
        }
    );

    impl TrayTarget {
        fn restore_window(&self) {
            let actions = self.ivars();
            let app = NSApplication::sharedApplication(self.mtm());
            if app.isHidden() {
                app.unhide(None);
            }
            if actions.window.isMiniaturized() {
                actions.window.deminiaturize(None);
            }
            (actions.restore)();
        }
    }

    pub struct Tray {
        bar: Retained<NSStatusBar>,
        item: Retained<NSStatusItem>,
        target: Retained<TrayTarget>,
        notifications: Retained<NSNotificationCenter>,
    }

    impl Tray {
        pub fn new(
            window: &gpui::Window,
            restore: impl Fn() + 'static,
            minimize: impl Fn() + 'static,
            quit: impl Fn() + 'static,
        ) -> Result<Self, String> {
            let mtm = MainThreadMarker::new().ok_or("菜单栏项目必须从主线程创建")?;
            let window = appkit_window(window).ok_or("找不到 AppKit 窗口")?;
            let menu = NSMenu::new(mtm);
            let target = TrayTarget::alloc(mtm).set_ivars(TrayActions {
                restore: Box::new(restore),
                minimize: Box::new(minimize),
                quit: Box::new(quit),
                window,
                menu: menu.clone(),
            });
            let target: Retained<TrayTarget> = unsafe { msg_send![super(target), init] };
            let image = NSImage::initWithData(
                NSImage::alloc(),
                &NSData::with_bytes(crate::assets::MENU_BAR_TEMPLATE),
            )
            .ok_or("无法加载听风吟菜单栏图标")?;
            let title = NSString::from_str("听风吟 BGM");
            image.setSize(NSSize::new(18., 18.));
            image.setTemplate(true);
            image.setAccessibilityDescription(Some(&title));
            let bar = NSStatusBar::systemStatusBar();
            let item = bar.statusItemWithLength(NSVariableStatusItemLength);
            let Some(button) = item.button(mtm) else {
                bar.removeStatusItem(&item);
                return Err("无法创建菜单栏按钮".into());
            };
            button.setToolTip(Some(&title));
            button.setImage(Some(&image));
            unsafe {
                for (label, action) in [("显示窗口", sel!(restore:)), ("退出听风吟", sel!(quit:))]
                {
                    menu.addItemWithTitle_action_keyEquivalent(
                        &NSString::from_str(label),
                        Some(action),
                        &NSString::from_str(""),
                    )
                    .setTarget(Some(target.as_ref()));
                }
                button.setTarget(Some(target.as_ref()));
                button.setAction(Some(sel!(clicked:)));
            }
            button.sendActionOn(NSEventMask::LeftMouseUp | NSEventMask::RightMouseDown);
            let notifications = NSNotificationCenter::defaultCenter();
            unsafe {
                notifications.addObserver_selector_name_object(
                    target.as_ref(),
                    sel!(minimized:),
                    Some(NSWindowDidMiniaturizeNotification),
                    Some(target.ivars().window.as_ref()),
                );
            }
            Ok(Self {
                bar,
                item,
                target,
                notifications,
            })
        }
    }

    impl Drop for Tray {
        fn drop(&mut self) {
            unsafe {
                self.notifications.removeObserver(self.target.as_ref());
            }
            self.bar.removeStatusItem(&self.item);
        }
    }

    pub fn set_autostart(enabled: bool) -> Result<(), String> {
        let directory = dirs::home_dir()
            .ok_or("找不到用户目录")?
            .join("Library/LaunchAgents");
        let program = enabled
            .then(std::env::current_exe)
            .transpose()
            .map_err(|error| error.to_string())?;
        write_login_agent(&directory, program.as_deref())
    }

    fn write_login_agent(directory: &Path, program: Option<&Path>) -> Result<(), String> {
        let file = directory.join(format!("{LAUNCH_ID}.plist"));
        let Some(program) = program else {
            return match fs::remove_file(&file) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.to_string()),
            };
        };
        let executable = program
            .to_str()
            .ok_or("应用路径无法写入启动项")?
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{LAUNCH_ID}</string>
<key>ProgramArguments</key><array><string>{executable}</string></array>
<key>RunAtLoad</key><true/>
<key>LimitLoadToSessionType</key><string>Aqua</string>
</dict></plist>"#
        );
        fs::create_dir_all(directory).map_err(|error| error.to_string())?;
        let mut temporary =
            tempfile::NamedTempFile::new_in(directory).map_err(|error| error.to_string())?;
        temporary
            .write_all(plist.as_bytes())
            .and_then(|_| {
                temporary
                    .persist(file)
                    .map(|_| ())
                    .map_err(|error| error.error)
            })
            .map_err(|error| format!("保存开机启动项失败：{error}"))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn login_agent_escapes_the_program_path_and_can_be_removed_twice() {
            let temporary = tempfile::tempdir().unwrap();
            let directory = temporary.path().join("LaunchAgents");
            let program = Path::new("/Applications/测试 & <BGM>.app/Contents/MacOS/tfy-bgm");
            write_login_agent(&directory, Some(program)).unwrap();
            let file = directory.join(format!("{LAUNCH_ID}.plist"));
            let result = std::process::Command::new("/usr/bin/plutil")
                .args(["-extract", "ProgramArguments.0", "raw", "-o", "-"])
                .arg(&file)
                .output()
                .unwrap();
            assert!(result.status.success());
            assert_eq!(
                String::from_utf8(result.stdout).unwrap().trim(),
                program.to_str().unwrap()
            );
            write_login_agent(&directory, None).unwrap();
            write_login_agent(&directory, None).unwrap();
            assert!(!file.exists());
        }
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: CFStringRef,
            level: u32,
            name: CFStringRef,
            id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
    }

    pub struct WakeLock(u32);

    impl WakeLock {
        pub fn acquire() -> Result<Self, String> {
            let kind = CFString::new("PreventUserIdleDisplaySleep");
            let name = CFString::new("听风吟 BGM 正在播放");
            let mut id = 0;
            let status = unsafe {
                IOPMAssertionCreateWithName(
                    kind.as_concrete_TypeRef(),
                    255,
                    name.as_concrete_TypeRef(),
                    &mut id,
                )
            };
            if status == 0 {
                Ok(Self(id))
            } else {
                Err(format!("无法保持屏幕唤醒（{status}）"))
            }
        }
    }

    impl Drop for WakeLock {
        fn drop(&mut self) {
            unsafe {
                IOPMAssertionRelease(self.0);
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod native {
    use super::SystemFont;

    pub fn reduce_motion() -> bool {
        false
    }

    pub struct Tray;
    pub struct WakeLock;

    pub fn edit_subtitle_text(
        _: &gpui::Window,
        _: &str,
        _: &str,
        _: Option<&str>,
    ) -> Result<tokio::sync::oneshot::Receiver<Option<String>>, String> {
        Err("当前平台不支持字幕文本编辑".into())
    }

    pub fn system_fonts(app: &gpui::App) -> Vec<SystemFont> {
        app.text_system()
            .all_font_names()
            .into_iter()
            .filter(|name| !name.starts_with(['.', '@']))
            .map(|family| SystemFont {
                label: family.clone(),
                family,
            })
            .collect()
    }

    pub fn window_visible(_: &gpui::Window) -> bool {
        true
    }

    impl Tray {
        pub fn new(
            _: &gpui::Window,
            _: impl Fn() + 'static,
            _: impl Fn() + 'static,
            _: impl Fn() + 'static,
        ) -> Result<Self, String> {
            Err("当前平台不支持菜单栏托盘".into())
        }
    }

    impl WakeLock {
        pub fn acquire() -> Result<Self, String> {
            Err("当前平台不支持防休眠".into())
        }
    }

    pub fn set_autostart(_: bool) -> Result<(), String> {
        Err("当前平台不支持开机启动".into())
    }
}

#[cfg(target_os = "macos")]
pub use native::{
    anchor_lyrics_settings, configure_lyrics_window, fit_lyrics_bounds, lyrics_pointer,
};

#[cfg(not(target_os = "macos"))]
pub fn anchor_lyrics_settings(_: &gpui::Window, _: &gpui::Window) {}

#[cfg(not(target_os = "macos"))]
pub fn configure_lyrics_window(_: &gpui::Window) {}

#[cfg(not(target_os = "macos"))]
pub fn fit_lyrics_bounds(_: &gpui::Window) {}

#[cfg(not(target_os = "macos"))]
pub fn lyrics_pointer(_: &gpui::Window, _: bool) -> bool {
    false
}

pub fn lyrics_window_size(window: &gpui::Window) -> (u16, u16) {
    let size = window.viewport_size();
    (
        f32::from(size.width).round() as u16,
        f32::from(size.height).round() as u16,
    )
}

#[cfg(target_os = "macos")]
pub use native::open_audio_file;
#[cfg(target_os = "macos")]
pub use native::set_unbundled_icon;

#[cfg(not(target_os = "macos"))]
pub fn open_audio_file(_: &std::path::Path) -> Result<(), String> {
    Err("当前平台不支持打开分离音频".into())
}
pub use native::{
    Tray, WakeLock, edit_subtitle_text, reduce_motion, set_autostart, system_fonts, window_visible,
};

pub fn wallpaper_window_active(window: &gpui::Window) -> bool {
    window.is_window_active() && window_visible(window)
}

#[cfg(target_os = "macos")]
pub fn unhide_and_focus_app() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    if let Some(mtm) = MainThreadMarker::new() {
        let app = NSApplication::sharedApplication(mtm);
        if app.isHidden() {
            app.unhide(None);
        }
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn unhide_and_focus_app() {}

#[cfg(unix)]
mod instance {
    use std::fs::{self, File, OpenOptions};
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use tokio::sync::mpsc::{self, UnboundedReceiver};

    pub struct SingleInstance {
        _lock: File,
        sock_path: PathBuf,
        wake_rx: Option<UnboundedReceiver<()>>,
    }

    pub enum AcquireOutcome {
        Primary(SingleInstance),
        AlreadyRunning,
    }

    impl SingleInstance {
        pub fn acquire(config_dir: &Path) -> Result<AcquireOutcome, String> {
            fs::create_dir_all(config_dir).map_err(|error| error.to_string())?;
            let lock_path = config_dir.join(".instance.lock");
            let lock = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&lock_path)
                .map_err(|error| error.to_string())?;
            if lock.try_lock().is_err() {
                let sock_path = config_dir.join(".instance.sock");
                if let Ok(mut stream) = UnixStream::connect(&sock_path) {
                    let _ = stream.set_write_timeout(Some(Duration::from_millis(400)));
                    let _ = stream.write_all(b"show\n");
                }
                return Ok(AcquireOutcome::AlreadyRunning);
            }
            let sock_path = config_dir.join(".instance.sock");
            let _ = fs::remove_file(&sock_path);
            let listener = UnixListener::bind(&sock_path).map_err(|error| error.to_string())?;
            let (wake_tx, wake_rx) = mpsc::unbounded_channel();
            std::thread::Builder::new()
                .name("tfy-single-instance".into())
                .spawn(move || {
                    for stream in listener.incoming().flatten() {
                        let mut stream = stream;
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(400)));
                        let mut buf = [0u8; 32];
                        if let Ok(n) = stream.read(&mut buf)
                            && buf[..n].starts_with(b"show")
                        {
                            let _ = wake_tx.send(());
                        }
                    }
                })
                .map_err(|error| error.to_string())?;
            Ok(AcquireOutcome::Primary(Self {
                _lock: lock,
                sock_path,
                wake_rx: Some(wake_rx),
            }))
        }

        pub fn take_receiver(&mut self) -> Option<UnboundedReceiver<()>> {
            self.wake_rx.take()
        }
    }

    impl Drop for SingleInstance {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.sock_path);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn single_instance_detects_second_launch_and_delivers_wake_signal() {
            let first_dir = tempfile::tempdir().unwrap();
            let second_dir = tempfile::tempdir().unwrap();
            let AcquireOutcome::Primary(mut guard) =
                SingleInstance::acquire(first_dir.path()).unwrap()
            else {
                panic!("expected primary lock");
            };
            let mut rx = guard.take_receiver().unwrap();
            assert!(matches!(
                SingleInstance::acquire(first_dir.path()).unwrap(),
                AcquireOutcome::AlreadyRunning
            ));
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap();
            rt.block_on(async {
                tokio::time::timeout(Duration::from_secs(2), rx.recv())
                    .await
                    .unwrap()
                    .unwrap();
            });
            assert!(matches!(
                SingleInstance::acquire(second_dir.path()).unwrap(),
                AcquireOutcome::Primary(_)
            ));
        }
    }
}

#[cfg(not(unix))]
mod instance {
    use std::path::Path;

    pub struct SingleInstance;

    pub enum AcquireOutcome {
        Primary(SingleInstance),
        AlreadyRunning,
    }

    impl SingleInstance {
        pub fn acquire(_: &Path) -> Result<AcquireOutcome, String> {
            Ok(AcquireOutcome::Primary(Self))
        }

        pub fn take_receiver(&mut self) -> Option<tokio::sync::mpsc::UnboundedReceiver<()>> {
            None
        }
    }
}

pub use instance::{AcquireOutcome as InstanceOutcome, SingleInstance};

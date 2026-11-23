pub(crate) mod mac_assoc {
    use std::ffi::{CStr, c_char, c_void};

    type CFStringRef = *const c_void;
    type OSStatus = i32;
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    /// kLSRolesAll。Viewer だけにすると、Editor として登録されたアプリが優先されることがある
    const K_LS_ROLES_ALL: u32 = 0xFFFF_FFFF;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithBytes(
            alloc: *const c_void,
            bytes: *const u8,
            num_bytes: isize,
            encoding: u32,
            is_external_representation: u8,
        ) -> CFStringRef;
        fn CFStringGetCString(s: CFStringRef, buf: *mut c_char, size: isize, encoding: u32) -> u8;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        static kUTTagClassFilenameExtension: CFStringRef;
        fn UTTypeCreatePreferredIdentifierForTag(
            tag_class: CFStringRef,
            tag: CFStringRef,
            conforming_to: CFStringRef,
        ) -> CFStringRef;
        fn LSSetDefaultRoleHandlerForContentType(
            content_type: CFStringRef,
            role: u32,
            handler_bundle_id: CFStringRef,
        ) -> OSStatus;
        fn LSCopyDefaultRoleHandlerForContentType(
            content_type: CFStringRef,
            role: u32,
        ) -> CFStringRef;
    }

    /// 解放を忘れないための CFString の入れ物
    struct CfString(CFStringRef);

    impl CfString {
        fn new(s: &str) -> Option<Self> {
            let r = unsafe {
                CFStringCreateWithBytes(
                    std::ptr::null(),
                    s.as_ptr(),
                    s.len() as isize,
                    K_CF_STRING_ENCODING_UTF8,
                    0,
                )
            };
            (!r.is_null()).then_some(CfString(r))
        }

        fn to_string(&self) -> Option<String> {
            let mut buf = [0 as c_char; 512];
            let ok = unsafe {
                CFStringGetCString(
                    self.0,
                    buf.as_mut_ptr(),
                    buf.len() as isize,
                    K_CF_STRING_ENCODING_UTF8,
                )
            };
            (ok != 0).then(|| {
                unsafe { CStr::from_ptr(buf.as_ptr()) }
                    .to_string_lossy()
                    .into_owned()
            })
        }
    }

    impl Drop for CfString {
        fn drop(&mut self) {
            unsafe { CFRelease(self.0) };
        }
    }

    /// 拡張子に対応する UTI（例: jpg → public.jpeg）
    fn uti_for_ext(ext: &str) -> Option<CfString> {
        let tag = CfString::new(ext)?;
        let r = unsafe {
            UTTypeCreatePreferredIdentifierForTag(
                kUTTagClassFilenameExtension,
                tag.0,
                std::ptr::null(),
            )
        };
        (!r.is_null()).then_some(CfString(r))
    }

    pub fn is_associated(ext: &str, bundle_id: &str) -> bool {
        let Some(uti) = uti_for_ext(ext) else {
            return false;
        };
        let r = unsafe { LSCopyDefaultRoleHandlerForContentType(uti.0, K_LS_ROLES_ALL) };
        if r.is_null() {
            return false;
        }
        CfString(r)
            .to_string()
            .is_some_and(|id| id.eq_ignore_ascii_case(bundle_id))
    }

    pub fn set_default(ext: &str, bundle_id: &str) -> Result<(), String> {
        let uti = uti_for_ext(ext).ok_or_else(|| format!(".{ext} の種類を特定できません"))?;
        let bundle = CfString::new(bundle_id).ok_or("バンドル ID を扱えません")?;
        let status =
            unsafe { LSSetDefaultRoleHandlerForContentType(uti.0, K_LS_ROLES_ALL, bundle.0) };
        if status == 0 {
            Ok(())
        } else {
            Err(format!(".{ext} を関連付けられません (OSStatus {status})"))
        }
    }
}

/// macOS: メインウィンドウへのカーソルの出入りと移動をフロントエンドへ知らせる。
///
/// WKWebView 自身の追跡範囲は「キーウィンドウのときだけ」有効なので、ウィンドウが
/// 非アクティブだと mousemove が届かず、アクティブでも外へ出たときの mouseleave が
/// 当てにならない（Windows の WebView2 はどちらも届く）。そこで content view に
/// 常時有効な NSTrackingArea を足し、出入りは `pointer-inside`（bool）、
/// 非アクティブ中の移動は `pointer-moved` として送る。アクティブ中の移動は
/// WebView の mousemove が届くので送らない
pub(crate) mod mac_pointer {
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{
        AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    };
    use objc2_app_kit::{NSEvent, NSTrackingArea, NSTrackingAreaOptions, NSWindow};
    use objc2_foundation::NSRect;
    use tauri::{AppHandle, Emitter, Manager, WebviewWindow};

    /// 非アクティブ中の移動を送る最短間隔（表示を出し直すだけなので粗くてよい）
    const MOVE_INTERVAL: Duration = Duration::from_millis(100);

    struct Ivars {
        app: AppHandle,
        /// 知らせる先のウィンドウのラベル
        label: String,
        window: Retained<NSWindow>,
        last_move: Cell<Option<Instant>>,
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "SViewPointerTracker"]
        #[ivars = Ivars]
        struct PointerTracker;

        impl PointerTracker {
            #[unsafe(method(mouseEntered:))]
            fn mouse_entered(&self, _event: &NSEvent) {
                self.emit_inside(true);
            }

            #[unsafe(method(mouseExited:))]
            fn mouse_exited(&self, _event: &NSEvent) {
                self.emit_inside(false);
            }

            #[unsafe(method(mouseMoved:))]
            fn mouse_moved(&self, _event: &NSEvent) {
                let ivars = self.ivars();
                if ivars.window.isKeyWindow() {
                    return;
                }
                let now = Instant::now();
                if ivars
                    .last_move
                    .get()
                    .is_some_and(|t| now.duration_since(t) < MOVE_INTERVAL)
                {
                    return;
                }
                ivars.last_move.set(Some(now));
                let _ = ivars.app.emit_to(ivars.label.as_str(), "pointer-moved", ());
            }
        }
    );

    impl PointerTracker {
        fn emit_inside(&self, inside: bool) {
            self.ivars().last_move.set(None);
            let ivars = self.ivars();
            let _ = ivars
                .app
                .emit_to(ivars.label.as_str(), "pointer-inside", inside);
        }
    }

    /// ビューアのウィンドウに追跡範囲を取り付ける。メインスレッドで呼ぶ
    pub fn install(window: &WebviewWindow) -> Result<(), String> {
        let mtm = MainThreadMarker::new().ok_or("メインスレッド以外から呼ばれました")?;
        let ptr = window
            .ns_window()
            .map_err(|e| format!("NSWindow を取得できません: {e}"))?;
        // SAFETY: Tauri が返すのは生きている NSWindow へのポインタ
        let ns_window = unsafe { Retained::retain(ptr.cast::<NSWindow>()) }
            .ok_or("NSWindow を取得できません")?;
        let view = ns_window
            .contentView()
            .ok_or("ウィンドウの content view がありません")?;

        let tracker = PointerTracker::alloc(mtm).set_ivars(Ivars {
            app: window.app_handle().clone(),
            label: window.label().to_string(),
            window: ns_window,
            last_move: Cell::new(None),
        });
        let tracker: Retained<PointerTracker> = unsafe { msg_send![super(tracker), init] };

        // InVisibleRect: 範囲はビューの見えている部分に自動で追従する（rect は無視される）
        let options = NSTrackingAreaOptions::MouseEnteredAndExited
            | NSTrackingAreaOptions::MouseMoved
            | NSTrackingAreaOptions::ActiveAlways
            | NSTrackingAreaOptions::InVisibleRect;
        let owner: &AnyObject = &tracker;
        // SAFETY: owner は mouseEntered: / mouseExited: / mouseMoved: を実装している
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                NSRect::ZERO,
                options,
                Some(owner),
                None,
            )
        };
        view.addTrackingArea(&area);
        // NSTrackingArea は owner を保持しないので、owner はわざと解放しない
        // （ウィンドウを閉じたあとに呼ばれても解放済みのものを触らないように。
        // ウィンドウ 1 枚につき小さなオブジェクト 1 つが残るだけ）
        std::mem::forget(tracker);
        Ok(())
    }
}

/// macOS で端・角を引っ張っている最中に、OS が大きさを決める前の段階で縦横比へ合わせる
/// （Windows の win_sizing にあたる）。tao のウィンドウの delegate に
/// `windowWillResize:toSize:` を足し、OS が出した大きさを縦横比に沿った大きさへ差し替える。
/// Resized が届いてから set_size で直すやり方は、tao の set_size が後から非同期で効くため、
/// OS の大きさとこちらの大きさが交互に描かれてちらつき、角では縦横比も崩れていた
pub(crate) mod mac_sizing {
    use std::sync::{Mutex, OnceLock};

    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::{msg_send, sel};
    use objc2_app_kit::{NSEvent, NSWindow, NSWindowStyleMask};
    use objc2_foundation::NSSize;
    use tauri::{AppHandle, LogicalSize, Manager, PhysicalSize, WebviewWindow};

    use crate::state::{AspectLock, LiveResize};
    use crate::window::{NO_LIMIT, sized_to_aspect, sizing_area};

    /// 引っ張り始めたカーソルが、ウィンドウの辺からこれ以内なら、その辺を引っ張っているとみなす
    /// （論理ピクセル）。左右・上下の両方の辺に近ければ角
    const EDGE_PX: f64 = 16.0;

    /// メソッドを足し終えたら入る（delegate のクラスはどのウィンドウも共通なので、足すのは 1 回だけ）
    static APP: OnceLock<AppHandle> = OnceLock::new();
    /// 取り付けたビューアのウィンドウ（NSWindow のアドレスとラベル）。delegate のクラスは
    /// 設定ウィンドウなどとも共通なので、呼ばれたのがどのビューアかをこれで見分ける。
    /// ウィンドウを閉じたら uninstall で外す
    static WINDOWS: Mutex<Vec<(usize, String)>> = Mutex::new(Vec::new());

    fn viewer_of(
        sender: *mut AnyObject,
    ) -> Option<(&'static AppHandle, String, &'static NSWindow)> {
        let app = APP.get()?;
        if sender.is_null() {
            return None;
        }
        let label = WINDOWS
            .lock()
            .ok()?
            .iter()
            .find(|(addr, _)| *addr == sender as usize)
            .map(|(_, label)| label.clone())?;
        // SAFETY: 一覧にあるのは閉じていないウィンドウだけで、delegate はメインスレッドで呼ばれる
        Some((app, label, unsafe { &*sender.cast::<NSWindow>() }))
    }

    /// 引っ張り始め。カーソルの位置から、引っ張られている辺を決めて覚える
    unsafe extern "C-unwind" fn will_start_live_resize(
        _this: *mut AnyObject,
        _sel: Sel,
        notification: *mut AnyObject,
    ) {
        let object: *mut AnyObject = unsafe { msg_send![notification, object] };
        let Some((app, label, window)) = viewer_of(object) else {
            return;
        };
        let mouse = NSEvent::mouseLocation();
        let frame = window.frame();
        let dx = (mouse.x - frame.origin.x).min(frame.origin.x + frame.size.width - mouse.x);
        let dy = (mouse.y - frame.origin.y).min(frame.origin.y + frame.size.height - mouse.y);
        // どちらの辺にも近くないとき（引っ張れる範囲が思ったより広いなど）は両方を候補にする
        let edges = match (dx <= EDGE_PX, dy <= EDGE_PX) {
            (false, false) => (true, true),
            edges => edges,
        };
        let content = window.contentRectForFrameRect(frame);
        app.state::<AspectLock>().with(&label, |state| {
            state.live = Some(LiveResize {
                edges,
                grab: LogicalSize::new(content.size.width, content.size.height),
                pulled: (false, false),
            });
        });
    }

    /// 引っ張っている最中。OS が出した大きさ（外枠）を縦横比に沿った大きさへ差し替えて返す
    unsafe extern "C-unwind" fn will_resize(
        _this: *mut AnyObject,
        _sel: Sel,
        sender: *mut AnyObject,
        size: NSSize,
    ) -> NSSize {
        let Some((app, label, window)) = viewer_of(sender) else {
            return size;
        };
        // 全画面への切り替え（とその解除）の間も呼ばれる。ここで縦横比へ合わせると
        // 画面いっぱいに広がれないので、OS の大きさのまま通す
        if window.styleMask().contains(NSWindowStyleMask::FullScreen) {
            return size;
        }
        let lock = app.state::<AspectLock>();
        // 取れないときは今回だけ OS に任せる。同じスレッドでロックを持ったままの処理
        // （ウィンドウの状態の問い合わせなど）の中から呼ばれることがあり、待つと落ちる
        let Ok(mut map) = lock.0.try_lock() else {
            return size;
        };
        let state = map.entry(label).or_default();
        let Some(ratio) = state.ratio else {
            return size;
        };
        // 渡されるのは外枠の大きさ。縦横比は中身に対して保つので、枠の分を除いて考える
        let frame = window.frame();
        let content = window.contentRectForFrameRect(frame);
        let border_w = frame.size.width - content.size.width;
        let border_h = frame.size.height - content.size.height;
        let now = LogicalSize::new(size.width - border_w, size.height - border_h);
        // 引っ張り始めが分からないときは、今の大きさを基準にする
        let mut live = state.live.unwrap_or(LiveResize {
            edges: (true, true),
            grab: LogicalSize::new(content.size.width, content.size.height),
            pulled: (false, false),
        });
        // 端だけを引っ張っているなら、もう一方の辺は OS が掴んだときの長さのまま渡してくる
        live.pulled.0 |= live.edges.0 && (now.width - live.grab.width).abs() >= 0.5;
        live.pulled.1 |= live.edges.1 && (now.height - live.grab.height).abs() >= 0.5;
        state.live = Some(live);
        let (horizontal, vertical) = live.pulled;
        if !horizontal && !vertical {
            return size;
        }
        let extra = state.extra;
        let area = sizing_area(horizontal, vertical, now, ratio, extra);
        let (width, height) = sized_to_aspect(ratio, extra, area, NO_LIMIT);
        // 続く Resized をドラッグ後の基準に使えるよう、ここで決めた大きさを覚えておく
        let fixed: PhysicalSize<u32> =
            LogicalSize::new(width, height).to_physical(window.backingScaleFactor());
        state.last = fixed;
        state.reported = fixed;
        state.area = width * height;
        NSSize::new(width + border_w, height + border_h)
    }

    /// 端・角を引っ張っている最中か（その間の Resized は keep_aspect_on_resize で直さない）。
    /// 取り付けに失敗していたら false（keep_aspect_on_resize に直させる）
    pub fn in_live_resize(window: &tauri::Window) -> bool {
        let Ok(ptr) = window.ns_window() else {
            return false;
        };
        let installed = WINDOWS
            .lock()
            .is_ok_and(|w| w.iter().any(|(addr, _)| *addr == ptr as usize));
        if !installed {
            return false;
        }
        // SAFETY: Resized はメインスレッドで届き、ptr は生きている NSWindow を指す
        unsafe { &*ptr.cast::<NSWindow>() }.inLiveResize()
    }

    unsafe fn add_method(
        class: *mut AnyClass,
        name: Sel,
        imp: Imp,
        types: &std::ffi::CStr,
    ) -> Result<(), String> {
        if unsafe { objc2::ffi::class_addMethod(class, name, imp, types.as_ptr()) }.as_bool() {
            Ok(())
        } else {
            Err(format!("{name} を追加できません（すでにあります）"))
        }
    }

    /// ビューアのウィンドウの delegate に、サイズ変更を縦横比に合わせる処理を足す。
    /// メインスレッドで呼ぶ
    pub fn install(window: &WebviewWindow) -> Result<(), String> {
        let ptr = window
            .ns_window()
            .map_err(|e| format!("NSWindow を取得できません: {e}"))?;
        // SAFETY: Tauri が返すのは生きている NSWindow へのポインタ
        let ns_window = unsafe { Retained::retain(ptr.cast::<NSWindow>()) }
            .ok_or("NSWindow を取得できません")?;
        let delegate: *mut AnyObject = unsafe { msg_send![&*ns_window, delegate] };
        if delegate.is_null() {
            return Err("ウィンドウの delegate がありません".to_string());
        }

        type WillResize =
            unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, NSSize) -> NSSize;
        type WillStart = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject);
        unsafe {
            let class = objc2::ffi::object_getClass(delegate) as *mut AnyClass;
            // 2 枚目以降のウィンドウは、足し済みのメソッドに気づかせるだけでよい
            if APP.get().is_none() {
                add_method(
                    class,
                    sel!(windowWillStartLiveResize:),
                    std::mem::transmute::<WillStart, Imp>(will_start_live_resize),
                    c"v@:@",
                )?;
                add_method(
                    class,
                    sel!(windowWillResize:toSize:),
                    std::mem::transmute::<WillResize, Imp>(will_resize),
                    c"{CGSize=dd}@:@{CGSize=dd}",
                )?;
                let _ = APP.set(window.app_handle().clone());
            }
            // NSWindow は delegate を設定したときに、delegate が応えるメソッドを調べて覚える。
            // 設定し直して、足したメソッドに気づかせる
            let _: () = msg_send![&*ns_window, setDelegate: std::ptr::null_mut::<AnyObject>()];
            let _: () = msg_send![&*ns_window, setDelegate: delegate];
        }
        // 足し終えてから、ビューアとして呼び分けを始める
        WINDOWS
            .lock()
            .map_err(|_| "ウィンドウの一覧を取得できません".to_string())?
            .push((
                Retained::as_ptr(&ns_window) as usize,
                window.label().to_string(),
            ));
        Ok(())
    }

    /// 閉じるウィンドウを呼び分けの対象から外す（同じアドレスが別のウィンドウに使い回されても
    /// 取り違えないように）
    pub fn uninstall(window: &tauri::Window) {
        let label = window.label();
        if let Ok(mut windows) = WINDOWS.lock() {
            windows.retain(|(_, l)| l != label);
        }
    }
}

/// macOS でウィンドウの移動を始める。
/// Tauri の start_dragging（tao）は、そのときの NSApp.currentEvent が押下のイベントでないと、
/// 画面上の座標をウィンドウ内の座標として入れた押下イベントを作って移動を始める。
/// 非アクティブのウィンドウを掴んだときがこれに当たり、ウィンドウの左上がカーソルへ飛んでいた。
/// ここではカーソルのウィンドウ内の座標を入れた押下イベントを作るので、掴んだ位置のまま動く
pub(crate) mod mac_drag {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_app_kit::{NSEvent, NSWindow};
    use objc2_foundation::NSPoint;

    /// NSEventTypeLeftMouseDown
    const LEFT_MOUSE_DOWN: usize = 1;

    /// メインスレッドから呼ぶ
    pub fn start(window: &NSWindow) {
        let mouse = NSEvent::mouseLocation();
        let frame = window.frame();
        // ウィンドウ内の座標は、外枠の左下を原点とする
        let location = NSPoint::new(mouse.x - frame.origin.x, mouse.y - frame.origin.y);
        unsafe {
            let info: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
            let timestamp: f64 = msg_send![info, systemUptime];
            let event: *mut AnyObject = msg_send![
                class!(NSEvent),
                mouseEventWithType: LEFT_MOUSE_DOWN,
                location: location,
                modifierFlags: 0usize,
                timestamp: timestamp,
                windowNumber: window.windowNumber(),
                context: std::ptr::null_mut::<AnyObject>(),
                eventNumber: 0isize,
                clickCount: 1isize,
                pressure: 1.0f32,
            ];
            if !event.is_null() {
                let _: () = msg_send![window, performWindowDragWithEvent: event];
            }
        }
    }
}

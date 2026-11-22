pub(crate) mod win_assoc {
    use std::path::{Path, PathBuf};
    use winreg::enums::{HKEY_CLASSES_ROOT, HKEY_CURRENT_USER};
    use winreg::RegKey;

    /// 「既定のアプリ」画面や RegisteredApplications に出る名前
    pub const APP_NAME: &str = "sView";
    const CAPABILITIES_KEY: &str = r"Software\sView\Capabilities";
    /// 種類ごとの ProgID と、エクスプローラーに出る種類の名前
    const PROG_IDS: &[(super::MediaKind, &str, &str)] = &[
        (
            super::MediaKind::Image,
            "sView.Image",
            "画像ファイル (sView)",
        ),
        (
            super::MediaKind::Video,
            "sView.Video",
            "動画ファイル (sView)",
        ),
        (
            super::MediaKind::Audio,
            "sView.Audio",
            "音楽ファイル (sView)",
        ),
    ];

    /// 拡張子に対応する sView の ProgID
    fn prog_id_for(ext: &str) -> &'static str {
        let kind = super::media_kind(Path::new(&format!("x.{ext}")));
        PROG_IDS
            .iter()
            .find(|(k, _, _)| Some(*k) == kind)
            .map(|(_, id, _)| *id)
            .unwrap_or(PROG_IDS[0].1)
    }

    #[link(name = "shell32")]
    extern "system" {
        fn SHChangeNotify(
            event_id: i32,
            flags: u32,
            item1: *const std::ffi::c_void,
            item2: *const std::ffi::c_void,
        );
    }

    fn reg_err(e: std::io::Error) -> String {
        format!("レジストリに書き込めません: {e}")
    }

    fn exe_path() -> Result<PathBuf, String> {
        std::env::current_exe().map_err(|e| format!("sView の場所が分かりません: {e}"))
    }

    /// ProgID の開くコマンドが sView の exe を指しているか
    fn prog_id_is_ours(prog_id: &str) -> bool {
        if PROG_IDS
            .iter()
            .any(|(_, id, _)| prog_id.eq_ignore_ascii_case(id))
        {
            return true;
        }
        let Some(exe) = exe_path()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()))
        else {
            return false;
        };
        RegKey::predef(HKEY_CLASSES_ROOT)
            .open_subkey(format!(r"{prog_id}\shell\open\command"))
            .and_then(|k| k.get_value::<String, _>(""))
            .map(|cmd| cmd.to_lowercase().contains(&exe))
            .unwrap_or(false)
    }

    /// いまエクスプローラーがこの拡張子に使う ProgID
    fn current_prog_id(ext: &str) -> Option<String> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let base = format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.{ext}");
        // 本人が「既定のアプリ」で選んだもの（Windows 11 の新しい版は UserChoiceLatest）
        for sub in ["UserChoiceLatest", "UserChoice"] {
            if let Ok(id) = hkcu
                .open_subkey(format!(r"{base}\{sub}"))
                .and_then(|k| k.get_value::<String, _>("ProgId"))
            {
                if !id.is_empty() {
                    return Some(id);
                }
            }
        }
        // 選んでいなければ、クラス登録の既定値が使われる
        RegKey::predef(HKEY_CLASSES_ROOT)
            .open_subkey(format!(".{ext}"))
            .and_then(|k| k.get_value::<String, _>(""))
            .ok()
            .filter(|id| !id.is_empty())
    }

    pub fn is_associated(ext: &str) -> bool {
        current_prog_id(ext).is_some_and(|id| prog_id_is_ours(&id))
    }

    /// sView を「開けるアプリ」として登録し直す。選ばれなかった拡張子の登録は外す
    pub fn register(exts: &[String], all: &[&str]) -> Result<(), String> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let exe = exe_path()?;
        let exe = exe.to_string_lossy();

        // ProgID（開き方）。インストーラーが HKLM に入れたものとは別に、ユーザー単位で持つ
        for &(_, id, name) in PROG_IDS {
            let (key, _) = hkcu
                .create_subkey(format!(r"Software\Classes\{id}"))
                .map_err(reg_err)?;
            key.set_value("", &name).map_err(reg_err)?;
            let (icon, _) = key.create_subkey("DefaultIcon").map_err(reg_err)?;
            icon.set_value("", &format!("\"{exe}\",0"))
                .map_err(reg_err)?;
            let (command, _) = key.create_subkey(r"shell\open\command").map_err(reg_err)?;
            command
                .set_value("", &format!("\"{exe}\" \"%1\""))
                .map_err(reg_err)?;
        }

        // 「既定のアプリ」画面に sView を出すための登録（Capabilities）
        let (caps, _) = hkcu.create_subkey(CAPABILITIES_KEY).map_err(reg_err)?;
        caps.set_value("ApplicationName", &APP_NAME)
            .map_err(reg_err)?;
        caps.set_value("ApplicationDescription", &"軽量な画像・動画・音楽ビューア")
            .map_err(reg_err)?;
        // 選ばれなかった拡張子を残さないよう、一覧は作り直す
        let _ = caps.delete_subkey_all("FileAssociations");
        let (assoc, _) = caps.create_subkey("FileAssociations").map_err(reg_err)?;
        for ext in exts {
            assoc
                .set_value(format!(".{ext}"), &prog_id_for(ext))
                .map_err(reg_err)?;
        }
        let (apps, _) = hkcu
            .create_subkey(r"Software\RegisteredApplications")
            .map_err(reg_err)?;
        apps.set_value(APP_NAME, &CAPABILITIES_KEY)
            .map_err(reg_err)?;

        // 「プログラムから開く」の候補
        for ext in all {
            let path = format!(r"Software\Classes\.{ext}\OpenWithProgids");
            if exts.iter().any(|e| e == ext) {
                let (key, _) = hkcu.create_subkey(&path).map_err(reg_err)?;
                key.set_value(prog_id_for(ext), &"").map_err(reg_err)?;
            } else if let Ok(key) =
                hkcu.open_subkey_with_flags(&path, winreg::enums::KEY_ALL_ACCESS)
            {
                let _ = key.delete_value(prog_id_for(ext));
            }
        }

        // エクスプローラーに関連付けの変更を知らせる（SHCNE_ASSOCCHANGED / SHCNF_IDLIST）
        unsafe { SHChangeNotify(0x0800_0000, 0, std::ptr::null(), std::ptr::null()) };
        Ok(())
    }

    /// Windows の「既定のアプリ」画面を sView の項目で開く
    /// （registeredAppUser が効かない古い Windows 10 では、既定のアプリの一覧が開く）
    pub fn open_default_apps_settings() -> Result<(), String> {
        std::process::Command::new("explorer")
            .arg(format!(
                "ms-settings:defaultapps?registeredAppUser={APP_NAME}"
            ))
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Windows の設定を開けません: {e}"))
    }
}

/// Windows: 「メディアに合わせる」で端・角を引っ張っている間、OS が大きさを
/// 決める前（WM_SIZING）に縦横比へ合わせる。
///
/// Resized を受けてから set_size で直すだけだと、マウスごとに「OS が決めた大きさ」と
/// 「直した大きさ」の 2 回描かれるうえ、角を引っ張ったときは直す基準の辺が
/// マウスの細かな動きで入れ替わり、ウィンドウと表示がちらつく。WM_SIZING なら
/// 引っ張られている辺・角が分かり、変わる前の四角形を書き換えられるので、
/// 縦横比どおりの大きさが 1 回だけ描かれる。ここで直した大きさは AspectLock に
/// 書いておくので、続いて届く Resized は keep_aspect_on_resize が跳ね返りとして見送る
pub(crate) mod win_sizing {
    use crate::state::AspectLock;
    use crate::window::{sized_to_aspect, sizing_area, NO_LIMIT};
    use tauri::{AppHandle, LogicalSize, Manager, PhysicalSize, WebviewWindow};

    type Hwnd = isize;
    type SubclassProc = unsafe extern "system" fn(Hwnd, u32, usize, isize, usize, usize) -> isize;

    #[repr(C)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[link(name = "comctl32")]
    extern "system" {
        fn SetWindowSubclass(hwnd: Hwnd, proc: SubclassProc, id: usize, data: usize) -> i32;
        fn DefSubclassProc(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize;
    }

    #[link(name = "user32")]
    extern "system" {
        fn GetWindowRect(hwnd: Hwnd, rect: *mut Rect) -> i32;
        fn GetClientRect(hwnd: Hwnd, rect: *mut Rect) -> i32;
        fn GetDpiForWindow(hwnd: Hwnd) -> u32;
    }

    const WM_SIZING: u32 = 0x0214;
    const WMSZ_LEFT: usize = 1;
    const WMSZ_RIGHT: usize = 2;
    const WMSZ_TOP: usize = 3;
    const WMSZ_TOPLEFT: usize = 4;
    const WMSZ_TOPRIGHT: usize = 5;
    const WMSZ_BOTTOM: usize = 6;
    const WMSZ_BOTTOMLEFT: usize = 7;
    const WMSZ_BOTTOMRIGHT: usize = 8;
    const SUBCLASS_ID: usize = 0x5356_4945; // "SVIE"

    /// サブクラスに持たせる、縦横比の覚え書きの引き先
    struct Target {
        app: AppHandle,
        label: String,
    }

    pub fn install(window: &WebviewWindow) -> Result<(), String> {
        let hwnd = window.hwnd().map_err(|e| e.to_string())?.0 as Hwnd;
        // サブクラスはウィンドウと同じだけ生きるので、AppHandle とラベルは手放さない
        let data = Box::into_raw(Box::new(Target {
            app: window.app_handle().clone(),
            label: window.label().to_string(),
        })) as usize;
        if unsafe { SetWindowSubclass(hwnd, proc, SUBCLASS_ID, data) } == 0 {
            drop(unsafe { Box::from_raw(data as *mut Target) });
            return Err("ウィンドウのサブクラス化に失敗しました".to_string());
        }
        Ok(())
    }

    unsafe extern "system" fn proc(
        hwnd: Hwnd,
        msg: u32,
        wparam: usize,
        lparam: isize,
        _id: usize,
        data: usize,
    ) -> isize {
        if msg == WM_SIZING && lparam != 0 && data != 0 {
            let target = &*(data as *const Target);
            if constrain(target, hwnd, wparam, &mut *(lparam as *mut Rect)) {
                return 1;
            }
        }
        DefSubclassProc(hwnd, msg, wparam, lparam)
    }

    /// 引っ張られている辺・角 edge に合わせて、これからなる四角形 rect（外枠・物理ピクセル）を
    /// 縦横比へ合わせる。反対側の辺・角は動かさない。書き換えたら true
    fn constrain(target: &Target, hwnd: Hwnd, edge: usize, rect: &mut Rect) -> bool {
        let (horizontal, vertical) = match edge {
            WMSZ_LEFT | WMSZ_RIGHT => (true, false),
            WMSZ_TOP | WMSZ_BOTTOM => (false, true),
            WMSZ_TOPLEFT | WMSZ_TOPRIGHT | WMSZ_BOTTOMLEFT | WMSZ_BOTTOMRIGHT => (true, true),
            _ => return false,
        };
        let lock = target.app.state::<AspectLock>();
        // 取れないときは今回だけ OS に任せる（あとの Resized で直る）
        let Ok(mut map) = lock.0.try_lock() else {
            return false;
        };
        let Some(state) = map.get_mut(&target.label) else {
            return false;
        };
        let Some(ratio) = state.ratio else {
            return false;
        };

        // 外枠と中身の差（枠なしでも Windows は見えない影の分だけ外枠が大きい）
        let mut outer = Rect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        let mut client = Rect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if unsafe { GetWindowRect(hwnd, &mut outer) } == 0
            || unsafe { GetClientRect(hwnd, &mut client) } == 0
        {
            return false;
        }
        let border_w = (outer.right - outer.left) - (client.right - client.left);
        let border_h = (outer.bottom - outer.top) - (client.bottom - client.top);
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        let scale = if dpi > 0 { dpi as f64 / 96.0 } else { 1.0 };

        let inner = PhysicalSize::new(
            (rect.right - rect.left - border_w).max(1) as u32,
            (rect.bottom - rect.top - border_h).max(1) as u32,
        );
        let now = inner.to_logical::<f64>(scale);
        let area = sizing_area(horizontal, vertical, now, ratio, state.extra);
        let (width, height) = sized_to_aspect(ratio, state.extra, area, NO_LIMIT);
        let fixed: PhysicalSize<u32> = LogicalSize::new(width, height).to_physical(scale);

        // 続く Resized が跳ね返りだと分かるように、ここで決めた大きさを覚えておく
        state.last = fixed;
        state.reported = fixed;
        state.area = width * height;
        drop(map);

        let w = fixed.width as i32 + border_w;
        let h = fixed.height as i32 + border_h;
        // 引っ張っている側を動かし、反対側は据え置く。
        // 辺のときは、縦横比で決まるもう一方は右・下へ伸び縮みさせる
        if matches!(edge, WMSZ_LEFT | WMSZ_TOPLEFT | WMSZ_BOTTOMLEFT) {
            rect.left = rect.right - w;
        } else {
            rect.right = rect.left + w;
        }
        if matches!(edge, WMSZ_TOP | WMSZ_TOPLEFT | WMSZ_TOPRIGHT) {
            rect.top = rect.bottom - h;
        } else {
            rect.bottom = rect.top + h;
        }
        true
    }
}

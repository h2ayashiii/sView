use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tauri::{LogicalSize, PhysicalPosition, PhysicalSize};

use crate::archive::*;
use crate::assoc::*;
use crate::audio::*;
use crate::listing::*;
use crate::logging::*;
use crate::media::*;
use crate::mkv::*;
use crate::settings::*;
use crate::state::*;
use crate::watcher::*;
use crate::window::*;

#[test]
fn association_exts_are_validated_and_normalized() {
    assert_eq!(
        normalize_association_exts(&[".JPG".into(), "png".into(), "jpg".into()]).unwrap(),
        vec!["jpg", "png"]
    );
    // 動画・音楽も関連付けられる
    assert_eq!(
        normalize_association_exts(&["MP4".into(), "flac".into()]).unwrap(),
        vec!["mp4", "flac"]
    );
    // 書庫は関連付けの対象にしない
    assert!(normalize_association_exts(&["zip".into()]).is_err());
    assert!(normalize_association_exts(&["cbz".into()]).is_err());
    assert!(normalize_association_exts(&["exe".into()]).is_err());
}

#[test]
fn clamp_to_area_keeps_window_inside_the_area() {
    let area = Area {
        x: 0.0,
        y: 0.0,
        width: 1920.0,
        height: 1040.0,
    };
    // 収まっていればそのまま
    assert_eq!(
        clamp_to_area(100.0, 100.0, 800.0, 600.0, &area),
        (100.0, 100.0)
    );
    // 右下にはみ出す → 右端・下端に寄せる
    assert_eq!(
        clamp_to_area(1500.0, 800.0, 800.0, 600.0, &area),
        (1120.0, 440.0)
    );
    // 左上にはみ出す
    assert_eq!(clamp_to_area(-50.0, -20.0, 800.0, 600.0, &area), (0.0, 0.0));
    // 領域より大きいときは左上を優先する
    assert_eq!(
        clamp_to_area(100.0, 100.0, 2500.0, 600.0, &area),
        (0.0, 100.0)
    );
}

#[test]
fn window_state_reads_old_files_and_keeps_kinds() {
    // 種類ごとの分が無い古い window.json もそのまま読める
    let old: WindowState =
        serde_json::from_str(r#"{"width":800,"height":600,"x":10,"y":20}"#).unwrap();
    assert_eq!(old.width, Some(800.0));
    assert!(old.kinds.is_empty());
    // 種類ごとの分が無ければ書き出しにも出さない
    assert!(!serde_json::to_string(&old).unwrap().contains("kinds"));

    let mut state = old.clone();
    let mut video = WindowState::default();
    copy_geometry(
        &mut video,
        &WindowState {
            width: Some(1280.0),
            height: Some(720.0),
            ..WindowState::default()
        },
    );
    state
        .kinds
        .insert(kind_key(WindowKindKey::Video).to_string(), video);
    let text = serde_json::to_string(&state).unwrap();
    let read: WindowState = serde_json::from_str(&text).unwrap();
    assert_eq!(read.kinds["video"].width, Some(1280.0));
    assert_eq!(read.kinds["video"].x, None);
    assert_eq!(read.x, Some(10.0));
}

#[test]
fn saved_position_follows_each_os_coordinates() {
    // 倍率 1.5 のディスプレイで論理 (1000, 200) に置いていたウィンドウ
    let state = WindowState {
        x: Some(1000.0),
        y: Some(200.0),
        scale: Some(1.5),
        ..WindowState::default()
    };
    // Windows（物理ピクセルが通しの座標）は記録したときの倍率で物理ピクセルへ戻す。
    // 今のウィンドウの倍率（1.0）で直すと別のディスプレイの位置になってしまう
    assert_eq!(saved_position(&state, 1.0, true), Some((1500.0, 300.0)));
    // macOS（ポイントが通しの座標）は論理ピクセルのまま
    assert_eq!(saved_position(&state, 1.0, false), Some((1000.0, 200.0)));
    // 倍率の無い古いファイルは今の倍率で読む（これまでと同じ）
    let old = WindowState {
        scale: None,
        ..state.clone()
    };
    assert_eq!(saved_position(&old, 2.0, true), Some((2000.0, 400.0)));
    // 位置が無ければ戻さない
    assert_eq!(saved_position(&WindowState::default(), 1.0, true), None);

    // 画面上の位置の座標から、ウィンドウの物理ピクセルへ
    assert_eq!(
        desktop_to_physical((1500.0, 300.0), 1.5, true),
        PhysicalPosition::new(1500, 300)
    );
    assert_eq!(
        desktop_to_physical((1000.0, 200.0), 2.0, false),
        PhysicalPosition::new(2000, 400)
    );
}

#[test]
fn copy_geometry_keeps_position_and_scale_together() {
    let mut state = WindowState {
        x: Some(10.0),
        y: Some(20.0),
        scale: Some(2.0),
        ..WindowState::default()
    };
    // 位置の取れなかった分は、前の位置と倍率を組のまま残す
    copy_geometry(
        &mut state,
        &WindowState {
            width: Some(800.0),
            height: Some(600.0),
            scale: Some(1.0),
            ..WindowState::default()
        },
    );
    assert_eq!(
        (state.x, state.y, state.scale),
        (Some(10.0), Some(20.0), Some(2.0))
    );
    assert_eq!(state.width, Some(800.0));
    copy_geometry(
        &mut state,
        &WindowState {
            x: Some(30.0),
            y: Some(40.0),
            scale: Some(1.25),
            ..WindowState::default()
        },
    );
    assert_eq!(
        (state.x, state.y, state.scale),
        (Some(30.0), Some(40.0), Some(1.25))
    );
}

#[test]
fn window_kind_key_includes_book_mode() {
    // フロントエンドは種類を小文字の文字列で渡す。本モードは画像とは別の枠で覚える
    for (text, key) in [
        ("image", WindowKindKey::Image),
        ("video", WindowKindKey::Video),
        ("audio", WindowKindKey::Audio),
        ("book", WindowKindKey::Book),
    ] {
        let parsed: WindowKindKey = serde_json::from_str(&format!("\"{text}\"")).unwrap();
        assert_eq!(parsed, key);
        assert_eq!(kind_key(key), text);
    }
}

fn sized(width: f64, height: f64, x: f64, y: f64) -> WindowState {
    WindowState {
        width: Some(width),
        height: Some(height),
        x: Some(x),
        y: Some(y),
        scale: Some(1.0),
        ..WindowState::default()
    }
}

#[test]
fn kind_target_uses_only_that_kinds_record() {
    // 最後に閉じたのは動画（横長）。画像の分は別に覚えている
    let mut saved = sized(1280.0, 720.0, 100.0, 100.0);
    saved
        .kinds
        .insert("video".into(), sized(1280.0, 720.0, 100.0, 100.0));
    saved
        .kinds
        .insert("image".into(), sized(600.0, 800.0, 300.0, 50.0));
    for first in [true, false] {
        assert_eq!(
            kind_target(&saved, WindowKindKey::Image, first),
            KindTarget::Record(sized(600.0, 800.0, 300.0, 50.0))
        );
    }
    // 覚えていない種類は、ほかの種類（動画）の大きさ・形を引き継がず既定の大きさにする
    for kind in [WindowKindKey::Audio, WindowKindKey::Book] {
        assert_eq!(kind_target(&saved, kind, true), KindTarget::Default);
        assert_eq!(kind_target(&saved, kind, false), KindTarget::Default);
    }
}

#[test]
fn kind_target_adopts_old_files_only_for_the_first_kind() {
    // 種類ごとの分が無い古い window.json。最初の種類だけは前回の姿を引き継ぐ
    let old = sized(1280.0, 720.0, 100.0, 100.0);
    assert_eq!(
        kind_target(&old, WindowKindKey::Image, true),
        KindTarget::Adopt
    );
    // 2 つ目の種類からは引き継がない（最初の種類の姿をそのまま使い回さない）
    assert_eq!(
        kind_target(&old, WindowKindKey::Video, false),
        KindTarget::Default
    );
    // 大きさの無い分は使えないので、既定の大きさにする
    let mut broken = old.clone();
    broken.kinds.insert(
        "image".into(),
        WindowState {
            x: Some(10.0),
            y: Some(10.0),
            ..WindowState::default()
        },
    );
    assert_eq!(
        kind_target(&broken, WindowKindKey::Image, true),
        KindTarget::Default
    );
}

#[test]
fn kind_target_record_does_not_carry_nested_state() {
    let mut record = sized(800.0, 600.0, 0.0, 0.0);
    record
        .kinds
        .insert("video".into(), sized(1.0, 1.0, 0.0, 0.0));
    let mut saved = WindowState::default();
    saved.kinds.insert("image".into(), record);
    let KindTarget::Record(target) = kind_target(&saved, WindowKindKey::Image, false) else {
        panic!("画像の分を使うはず");
    };
    assert!(target.kinds.is_empty());
    assert_eq!((target.width, target.height), (Some(800.0), Some(600.0)));
}

#[test]
fn window_state_keeps_the_fit_area_of_each_kind() {
    // 縦長の画像で画面に収めたため、ウィンドウは広さより小さい
    let mut record = sized(540.0, 960.0, 10.0, 20.0);
    record.area = Some(1280.0 * 720.0);
    let mut state = WindowState::default();
    state.store(WindowSlot::Kind(WindowKindKey::Image), &record);
    let text = serde_json::to_string(&state).unwrap();
    let read: WindowState = serde_json::from_str(&text).unwrap();
    let KindTarget::Record(target) = kind_target(&read, WindowKindKey::Image, false) else {
        panic!("画像の分を使うはず");
    };
    // 広さは大きさから求め直さず、覚えていた値で戻す
    assert_eq!(target.fit_area(), 1280.0 * 720.0);
    // 広さの無い古い記録は大きさから求める
    assert_eq!(sized(800.0, 600.0, 0.0, 0.0).fit_area(), 800.0 * 600.0);
    // 「自由に変更」の枠（最上位）には広さを書かない
    state.store(WindowSlot::Free, &record);
    assert_eq!(state.area, None);
    assert!(
        !serde_json::to_string(&WindowState::default())
            .unwrap()
            .contains("area")
    );
    // 以前の「最後に使った種類」（kind）が残っていても読める
    let old: WindowState =
        serde_json::from_str(r#"{"width":800,"height":600,"kind":"video"}"#).unwrap();
    assert_eq!(old.width, Some(800.0));
}

#[test]
fn kind_state_targets_one_slot_when_free() {
    let mut state = KindState::default();
    // 何も表示していない（ファイルを指定せずに起動しただけの）ウィンドウはどの枠でもない
    assert_eq!(state.target(), None);
    assert!(!state.pending());
    state.applied = Some(WindowSlot::Free);
    // 「自由に変更」は種類によらず 1 つの枠。種類が変わっても置き直さない
    for kind in [
        WindowKindKey::Image,
        WindowKindKey::Video,
        WindowKindKey::Audio,
        WindowKindKey::Book,
    ] {
        state.shown = Some(kind);
        assert_eq!(state.target(), Some(WindowSlot::Free));
        assert!(!state.pending());
    }
}

#[test]
fn kind_state_targets_the_shown_kind_when_fitting() {
    let mut state = KindState {
        fit: true,
        ..KindState::default()
    };
    // 何も表示していなければ使う枠は決まらない
    assert_eq!(state.target(), None);
    assert!(!state.pending());
    state.applied = Some(WindowSlot::Kind(WindowKindKey::Video));
    state.shown = Some(WindowKindKey::Video);
    assert!(!state.pending());
    // 最大化中に画像を表示した → 普段の姿はまだ動画の枠のもの
    state.shown = Some(WindowKindKey::Image);
    assert_eq!(state.target(), Some(WindowSlot::Kind(WindowKindKey::Image)));
    assert!(state.pending());
    state.applied = Some(WindowSlot::Kind(WindowKindKey::Image));
    assert!(!state.pending());
    // 「自由に変更」へ切り替えたら、1 つの枠へ置き直すまで待ち
    state.fit = false;
    assert!(state.pending());
}

#[test]
fn window_state_store_keeps_free_and_kinds_apart() {
    let mut state = sized(800.0, 600.0, 10.0, 20.0);
    // 「メディアに合わせる」の種類の枠に書いても、「自由に変更」の枠（最上位）は変わらない
    state.store(
        WindowSlot::Kind(WindowKindKey::Video),
        &sized(1280.0, 720.0, 50.0, 60.0),
    );
    assert_eq!((state.width, state.height), (Some(800.0), Some(600.0)));
    assert_eq!((state.x, state.y), (Some(10.0), Some(20.0)));
    assert_eq!(state.kinds["video"], sized(1280.0, 720.0, 50.0, 60.0));
    // 「自由に変更」の枠に書いても、種類ごとの枠は変わらない
    state.store(WindowSlot::Free, &sized(640.0, 480.0, 0.0, 0.0));
    assert_eq!((state.width, state.height), (Some(640.0), Some(480.0)));
    assert_eq!(state.kinds["video"], sized(1280.0, 720.0, 50.0, 60.0));
    // 種類ごとの枠には入れ子を持たせない
    let mut nested = sized(300.0, 300.0, 0.0, 0.0);
    nested
        .kinds
        .insert("audio".into(), sized(1.0, 1.0, 0.0, 0.0));
    state.store(WindowSlot::Kind(WindowKindKey::Image), &nested);
    assert_eq!(state.kinds["image"], sized(300.0, 300.0, 0.0, 0.0));
}

#[test]
fn startup_listing_is_used_once_for_the_same_open() {
    let dir = std::env::temp_dir().join(format!("sview-startup-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("sub")).unwrap();
    fs::write(dir.join("b.png"), b"").unwrap();
    fs::write(dir.join("a.mp4"), b"").unwrap();
    fs::write(dir.join("sub").join("c.mp3"), b"").unwrap();
    let path = dir.to_string_lossy().into_owned();
    let cache = ArchiveCache(Mutex::new(None));
    // フォルダは並べたとき先頭に来るファイルの種類
    let listing = list_path(&path, None, None, &cache).unwrap();
    assert_eq!(listing.kind, Some(MediaKind::Video));
    let images = listing.images.clone();

    // 同じパスを最初の条件で開くときだけ、預けた一覧をそのまま渡す
    let mut slot = Some((path.clone(), Ok(listing)));
    let taken = take_startup_listing(&mut slot, &path, None, None).unwrap();
    assert_eq!(taken.unwrap().images, images);
    // 一度渡したら空（読み直しなどは毎回読む）
    assert!(slot.is_none());
    assert!(take_startup_listing(&mut slot, &path, None, None).is_none());

    // 別のパス・読み直し（種類や深さの指定あり）には使わず、捨てる
    for (other, kind, depth) in [
        ("other", None, None),
        (path.as_str(), Some(MediaKind::Video), None),
        (path.as_str(), None, Some(MAX_DEPTH)),
    ] {
        let mut slot = Some((path.clone(), list_path(&path, None, None, &cache)));
        assert!(take_startup_listing(&mut slot, other, kind, depth).is_none());
        assert!(slot.is_none());
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn clamp_to_area_uses_the_area_origin() {
    // 右側に並べた 2 台目のディスプレイ
    let area = Area {
        x: 1920.0,
        y: 0.0,
        width: 1280.0,
        height: 720.0,
    };
    assert_eq!(
        clamp_to_area(3000.0, 500.0, 800.0, 600.0, &area),
        (2400.0, 120.0)
    );
    assert_eq!(
        clamp_to_area(1800.0, 10.0, 800.0, 600.0, &area),
        (1920.0, 10.0)
    );
}

#[test]
fn fitted_position_keeps_the_center() {
    let pos = fitted_position(
        PhysicalPosition::new(300, 200),
        PhysicalSize::new(1000, 700),
        PhysicalSize::new(1000, 700),
        PhysicalSize::new(600, 300),
        None,
        None,
    );
    assert_eq!(pos, PhysicalPosition::new(500, 400));
}

#[test]
fn fitted_position_does_not_drift_with_hidden_frame() {
    // Windows の枠なしウィンドウは影のぶん外枠が中身より大きい（ここでは 16 x 8）。
    // 大きさの違う 2 枚を何度行き来しても左上が元に戻ること（右下へずれていかない）
    let frame = (16, 8);
    let a = PhysicalSize::new(1000, 700);
    let b = PhysicalSize::new(801, 903);
    let mut pos = PhysicalPosition::new(300, 200);
    let mut inner = a;
    for _ in 0..10 {
        for next in [b, a] {
            let outer = PhysicalSize::new(inner.width + frame.0, inner.height + frame.1);
            pos = fitted_position(pos, outer, inner, next, None, None);
            inner = next;
        }
    }
    assert_eq!(pos, PhysicalPosition::new(300, 200));
}

#[test]
fn fitted_position_stays_inside_the_work_area() {
    // 小さなウィンドウの中心を保ったまま大きな画像に合わせるとはみ出すので、
    // 作業領域の中へ寄せる
    let area = Area {
        x: 0.0,
        y: 0.0,
        width: 1920.0,
        height: 1040.0,
    };
    let pos = fitted_position(
        PhysicalPosition::new(1200, 700),
        PhysicalSize::new(400, 300),
        PhysicalSize::new(400, 300),
        PhysicalSize::new(1600, 900),
        None,
        Some(&area),
    );
    assert_eq!(pos, PhysicalPosition::new(320, 140));
}

#[test]
fn fitted_position_uses_the_anchor_for_the_first_image() {
    let area = Area {
        x: 0.0,
        y: 0.0,
        width: 1920.0,
        height: 1040.0,
    };
    let default_size = PhysicalSize::new(960, 640);
    // 前回の左上にそのまま置く
    let pos = fitted_position(
        PhysicalPosition::new(0, 0),
        default_size,
        default_size,
        PhysicalSize::new(800, 600),
        Some(PhysicalPosition::new(500, 300)),
        Some(&area),
    );
    assert_eq!(pos, PhysicalPosition::new(500, 300));
    // 前回の左上だとはみ出す大きさなら寄せる
    let pos = fitted_position(
        PhysicalPosition::new(0, 0),
        default_size,
        default_size,
        PhysicalSize::new(1600, 900),
        Some(PhysicalPosition::new(500, 300)),
        Some(&area),
    );
    assert_eq!(pos, PhysicalPosition::new(320, 140));
}

#[test]
fn sized_to_aspect_keeps_the_area_and_takes_the_ratio_from_the_image() {
    let limit = (1728.0, 972.0);
    // 500x500 相当の広さで 16:9 の画像に合わせる → 広さはそのまま、形だけ画像に合う
    let (w, h) = sized_to_aspect(16.0 / 9.0, (0.0, 0.0), 500.0 * 500.0, limit);
    assert!((w / h - 16.0 / 9.0).abs() < 1e-9);
    assert!((w * h - 250_000.0).abs() < 1e-6);
    // 縦長の画像でも広さは変わらない（切り替えても大きさの印象が揃う）
    let (w, h) = sized_to_aspect(9.0 / 16.0, (0.0, 0.0), 500.0 * 500.0, limit);
    assert!((w / h - 9.0 / 16.0).abs() < 1e-9);
    assert!((w * h - 250_000.0).abs() < 1e-6);
}

#[test]
fn dragged_area_keeps_the_edge_that_was_pulled() {
    let limit = (1728.0, 972.0);
    let ratio = 16.0 / 9.0;
    let before = LogicalSize::new(800.0, 450.0);
    // 右端を引っ張った（横だけ変わった）→ 横はそのまま、縦が縦横比で決まる
    let now = LogicalSize::new(1000.0, 450.0);
    let (w, h) = sized_to_aspect(
        ratio,
        (0.0, 0.0),
        dragged_area(now, before, ratio, (0.0, 0.0)),
        limit,
    );
    assert!((w - 1000.0).abs() < 1e-9);
    assert!((h - 1000.0 / ratio).abs() < 1e-9);
    // 下端を引っ張った（縦だけ変わった）→ 縦はそのまま、横が縦横比で決まる
    let now = LogicalSize::new(800.0, 600.0);
    let (w, h) = sized_to_aspect(
        ratio,
        (0.0, 0.0),
        dragged_area(now, before, ratio, (0.0, 0.0)),
        limit,
    );
    assert!((h - 600.0).abs() < 1e-9);
    assert!((w - 600.0 * ratio).abs() < 1e-9);
}

#[test]
fn sizing_area_follows_the_pulled_edge_and_is_continuous_at_corners() {
    let limit = (1728.0, 972.0);
    let ratio = 16.0 / 9.0;
    let size = |h, v, w, ht| {
        sized_to_aspect(
            ratio,
            (0.0, 0.0),
            sizing_area(h, v, LogicalSize::new(w, ht), ratio, (0.0, 0.0)),
            limit,
        )
    };
    // 左右の辺: 横はそのまま、縦が縦横比で決まる
    let (w, h) = size(true, false, 1000.0, 450.0);
    assert!((w - 1000.0).abs() < 1e-9 && (h - 1000.0 / ratio).abs() < 1e-9);
    // 上下の辺: 縦はそのまま、横が縦横比で決まる
    let (w, h) = size(false, true, 800.0, 600.0);
    assert!((h - 600.0).abs() < 1e-9 && (w - 600.0 * ratio).abs() < 1e-9);
    // 角: 大きい方に合わせる（縦横比から外れた位置でも、縦横比のまま覆う大きさ）
    let (w, h) = size(true, true, 1000.0, 600.0);
    assert!((w - 600.0 * ratio).abs() < 1e-9 && (h - 600.0).abs() < 1e-9);
    // 角: マウスが少し動いただけなら大きさも少ししか変わらない（どちらの辺が
    // 大きく動いたかで行き来しない）
    let (w1, _) = size(true, true, 1000.0, 562.0);
    let (w2, _) = size(true, true, 1001.0, 562.5);
    assert!((w2 - w1).abs() < 2.0);
}

#[test]
fn sized_to_aspect_keeps_the_ratio_outside_the_fixed_margin() {
    let limit = (1728.0, 972.0);
    // 音楽: アートワークの周りの余白（横 56、縦 186）はウィンドウの大きさによらず一定。
    // 縦横比は余白を除いた部分で保ち、広さはウィンドウ全体で保つ
    let extra = (56.0, 186.0);
    for aspect in [1.0, 2.0 / 3.0, 16.0 / 9.0] {
        let (w, h) = sized_to_aspect(aspect, extra, 500.0 * 700.0, limit);
        assert!(((w - extra.0) / (h - extra.1) - aspect).abs() < 1e-9);
        assert!((w * h - 350_000.0).abs() < 1e-6);
    }
    // 横を引っ張ったら横はそのまま、縦は余白を除いた部分の縦横比で決まる
    let before = LogicalSize::new(500.0, 700.0);
    let now = LogicalSize::new(600.0, 700.0);
    let (w, h) = sized_to_aspect(1.0, extra, dragged_area(now, before, 1.0, extra), limit);
    assert!((w - 600.0).abs() < 1e-6);
    assert!((h - (600.0 - 56.0 + 186.0)).abs() < 1e-6);
    // 画面に収めるときも、余白を除いた部分の縦横比は崩さない
    let (w, h) = sized_to_aspect(1.0, extra, 4000.0 * 4000.0, limit);
    assert!(w <= limit.0 + 1e-9 && h <= limit.1 + 1e-9);
    assert!(((w - extra.0) / (h - extra.1) - 1.0).abs() < 1e-9);
}

#[test]
fn within_limit_shrinks_keeping_the_ratio() {
    let limit = (1824.0, 988.0);
    // 収まっていればそのまま
    assert_eq!(within_limit((960.0, 640.0), limit), (960.0, 640.0));
    // はみ出す分は縦横比を保って縮める（縦が決め手）
    let (w, h) = within_limit((1200.0, 2000.0), limit);
    assert!((h - 988.0).abs() < 1e-9);
    assert!((w / h - 0.6).abs() < 1e-9);
    // 制限が無ければそのまま
    assert_eq!(within_limit((5000.0, 4000.0), NO_LIMIT), (5000.0, 4000.0));
}

#[test]
fn centered_in_area_uses_the_work_area() {
    let area = Area {
        x: 1920.0,
        y: 40.0,
        width: 1920.0,
        height: 1040.0,
    };
    assert_eq!(centered_in_area(960.0, 640.0, &area), (2400.0, 240.0));
    // 作業領域より大きければ左上に合わせる
    assert_eq!(centered_in_area(2000.0, 1200.0, &area), (1920.0, 40.0));
}

#[test]
fn sized_to_aspect_shrinks_to_fit_the_screen() {
    let limit = (1728.0, 972.0);
    // 画面に収まらない広さを求められたら、縦横比を保ったまま縮める
    let (w, h) = sized_to_aspect(1.0, (0.0, 0.0), 4000.0 * 4000.0, limit);
    assert!((w - 972.0).abs() < 1e-9);
    assert!((h - 972.0).abs() < 1e-9);
}

#[test]
fn sized_to_aspect_grows_to_the_minimum_size() {
    let limit = (1728.0, 972.0);
    // 小さすぎる指定でも最小サイズは下回らない
    let (w, h) = sized_to_aspect(1.0, (0.0, 0.0), 1.0, limit);
    assert!(w >= MIN_WINDOW_SIZE.0 - 1e-9);
    assert!(h >= MIN_WINDOW_SIZE.1 - 1e-9);
    assert!((w / h - 1.0).abs() < 1e-9);
}

#[test]
fn merge_settings_overwrites_only_the_given_keys() {
    use serde_json::json;
    // ほかのプロセスで変えた項目（backgroundOpacity）は残し、渡した項目だけを書き換える
    let current = json!({ "audioVolume": 40, "backgroundOpacity": 70, "old": true });
    let merged = merge_settings(Some(current), json!({ "audioVolume": 55 }));
    assert_eq!(
        merged,
        json!({ "audioVolume": 55, "backgroundOpacity": 70, "old": true })
    );
    // 保存済みが無い・壊れているときは渡した項目だけ
    assert_eq!(merge_settings(None, json!({ "a": 1 })), json!({ "a": 1 }));
    assert_eq!(
        merge_settings(Some(json!([1, 2])), json!({ "a": 1 })),
        json!({ "a": 1 })
    );
}

#[test]
fn natural_sort_orders_numbers_numerically() {
    let mut v = vec!["img10.png", "img2.png", "img1.png", "IMG3.png"];
    v.sort_by(|a, b| natural_cmp(a, b));
    assert_eq!(v, vec!["img1.png", "img2.png", "IMG3.png", "img10.png"]);
}

#[test]
fn natural_sort_handles_leading_zeros_and_plain_names() {
    let mut v = vec!["b.png", "a002.png", "a1.png", "a.png"];
    v.sort_by(|a, b| natural_cmp(a, b));
    assert_eq!(v, vec!["a.png", "a1.png", "a002.png", "b.png"]);
}

#[test]
fn image_extension_detection() {
    assert!(is_image(Path::new("/x/photo.JPG")));
    assert!(is_image(Path::new("/x/photo.webp")));
    assert!(!is_image(Path::new("/x/notes.txt")));
    assert!(!is_image(Path::new("/x/noext")));
}

#[test]
fn video_extension_detection() {
    assert!(is_video(Path::new("/x/clip.MP4")));
    assert!(is_video(Path::new("/x/clip.webm")));
    assert!(!is_video(Path::new("/x/clip.avi")));
    assert!(is_video(Path::new("/x/clip.MKV")));
    assert!(!is_video(Path::new("/x/photo.png")));
    assert!(is_media(Path::new("/x/clip.mov")));
    assert!(is_media(Path::new("/x/photo.png")));
    assert!(!is_media(Path::new("/x/notes.txt")));
}

#[test]
fn audio_extension_detection() {
    assert!(is_audio(Path::new("/x/song.MP3")));
    assert!(is_audio(Path::new("/x/song.flac")));
    assert!(is_audio(Path::new("/x/song.m4a")));
    assert!(!is_audio(Path::new("/x/song.wma")));
    assert!(!is_audio(Path::new("/x/clip.mp4")));
    assert!(is_media(Path::new("/x/song.ogg")));
}

/// 埋め込みのアートワークが無いときは、同じフォルダのジャケット画像を
/// COVER_STEMS の順（cover → folder → …）で選ぶ。大文字小文字は問わない
#[test]
fn folder_artwork_prefers_cover_names_in_order() {
    let dir = std::env::temp_dir().join(format!("sview-cover-test-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let song = dir.join("01 song.mp3");
    fs::write(&song, b"x").unwrap();
    assert_eq!(folder_artwork(&song), None);
    fs::write(dir.join("booklet.jpg"), b"booklet").unwrap();
    assert_eq!(folder_artwork(&song), None);
    fs::write(dir.join("Folder.JPG"), b"folder").unwrap();
    assert_eq!(folder_artwork(&song).as_deref(), Some(&b"folder"[..]));
    fs::write(dir.join("cover.png"), b"cover").unwrap();
    assert_eq!(folder_artwork(&song).as_deref(), Some(&b"cover"[..]));
    fs::remove_dir_all(&dir).ok();
}

/// タグに埋め込まれたアートワークと曲名を読める。表紙（CoverFront）を優先する
#[test]
fn reads_embedded_tags_and_artwork() {
    use lofty::config::WriteOptions;
    use lofty::picture::{MimeType, Picture, PictureType};
    use lofty::prelude::*;
    use lofty::tag::{Tag, TagType};

    let dir = std::env::temp_dir().join(format!("sview-embed-test-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let song = dir.join("song.mp3");
    // MPEG-1 Layer III / 128 kbps / 44.1 kHz のフレーム（中身は無音でよい）を並べる
    let mut frame = vec![0xFF, 0xFB, 0x90, 0x00];
    frame.resize(417, 0);
    fs::write(&song, frame.repeat(4)).unwrap();
    // 同じフォルダのジャケット画像より、埋め込みの方を先に使う
    fs::write(dir.join("cover.jpg"), b"folder cover").unwrap();

    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title("曲名".to_string());
    tag.set_artist("アーティスト".to_string());
    let picture = |kind, data: &[u8]| {
        Picture::unchecked(data.to_vec())
            .pic_type(kind)
            .mime_type(MimeType::Png)
            .build()
    };
    tag.push_picture(picture(PictureType::Other, b"other"));
    tag.push_picture(picture(PictureType::CoverFront, b"front"));
    tag.save_to_path(&song, WriteOptions::default()).unwrap();

    let path = song.to_string_lossy().into_owned();
    let info = audio_info(path).unwrap();
    assert_eq!(info.title.as_deref(), Some("曲名"));
    assert_eq!(info.artist.as_deref(), Some("アーティスト"));
    assert_eq!(info.album, None);
    assert_eq!(embedded_artwork(&song).as_deref(), Some(&b"front"[..]));
    fs::remove_dir_all(&dir).ok();
}

/// タグの無い（壊れた）音楽ファイルでもエラーにせず、空の情報を返す
#[test]
fn audio_info_tolerates_missing_tags() {
    let dir = std::env::temp_dir().join(format!("sview-tag-test-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let song = dir.join("broken.mp3");
    fs::write(&song, b"not really an mp3").unwrap();
    let info = audio_info(song.to_string_lossy().into_owned()).unwrap();
    assert_eq!((info.title, info.artist, info.album), (None, None, None));
    assert!(audio_info(dir.join("x.png").to_string_lossy().into_owned()).is_err());
    fs::remove_dir_all(&dir).ok();
}

/// 一覧のファイル名だけを取り出す
fn file_names(list: &ImageList) -> Vec<String> {
    list.images
        .iter()
        .map(|p| {
            Path::new(p)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

fn test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sview-{name}-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// 単体のファイルから開くと、同じフォルダの直下にある同じ種類のファイルだけを並べる
#[test]
fn single_file_lists_only_its_own_kind() {
    let dir = test_dir("single");
    for name in [
        "a10.png",
        "a2.mp4",
        "a1.jpg",
        "a3.webm",
        "notes.txt",
        "a4.avi",
        "a5.mp3",
        "a6.wma",
    ] {
        fs::write(dir.join(name), b"x").unwrap();
    }
    fs::create_dir_all(dir.join("sub")).unwrap();
    fs::write(dir.join("sub/a0.png"), b"x").unwrap();

    let list = list_dir_images(&dir, 1, Some(MediaKind::Video), Some("a3.webm".as_ref())).unwrap();
    assert_eq!(file_names(&list), vec!["a2.mp4", "a3.webm"]);
    assert_eq!(list.index, 1);
    assert_eq!(list.kind, Some(MediaKind::Video));
    assert_eq!(list.prefix, "");

    // サブフォルダの画像は拾わない
    let list = list_dir_images(&dir, 1, Some(MediaKind::Image), Some("a10.png".as_ref())).unwrap();
    assert_eq!(file_names(&list), vec!["a1.jpg", "a10.png"]);
    assert_eq!(list.index, 1);

    let list = list_dir_images(&dir, 1, Some(MediaKind::Audio), Some("a5.mp3".as_ref())).unwrap();
    assert_eq!(file_names(&list), vec!["a5.mp3"]);
    fs::remove_dir_all(&dir).ok();
}

/// フォルダを開くと 3 層目まで読み、並べたとき先頭に来るファイルの種類だけを残す
#[test]
fn folder_listing_keeps_the_first_kind_down_to_three_levels() {
    let dir = test_dir("folder");
    for name in [
        "b.mp4",
        "a/1.mp3",
        "a/x/2.png",
        "a/x/y/3.png",
        "c.png",
        "notes.txt",
    ] {
        let path = dir.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"x").unwrap();
    }

    // 相対パスの自然順で a/1.mp3 が先頭なので、音楽だけ
    let list = list_dir_images(&dir, MAX_DEPTH, None, None).unwrap();
    assert_eq!(file_names(&list), vec!["1.mp3"]);
    assert_eq!(list.kind, Some(MediaKind::Audio));
    assert_eq!(list.depth, MAX_DEPTH);
    assert!(list.prefix.ends_with(std::path::MAIN_SEPARATOR));

    // 読み直しでは最初に決めた種類を保つ。
    // a/x/2.png は 3 層目なので読み、a/x/y/3.png は 4 層目なので読まない
    let list = list_dir_images(&dir, MAX_DEPTH, Some(MediaKind::Image), None).unwrap();
    assert_eq!(file_names(&list), vec!["2.png", "c.png"]);

    // 空のフォルダは種類が決まらない
    let empty = test_dir("folder-empty");
    let list = list_dir_images(&empty, MAX_DEPTH, None, None).unwrap();
    assert!(list.images.is_empty());
    assert_eq!(list.kind, None);

    fs::remove_dir_all(&dir).ok();
    fs::remove_dir_all(&empty).ok();
}

#[test]
fn listing_change_only_reacts_to_images_appearing_or_disappearing() {
    use notify::event::{CreateKind, DataChange, EventKind, ModifyKind, RemoveKind, RenameMode};
    let event = |kind, paths: &[&str]| notify::Event {
        kind,
        paths: paths.iter().map(PathBuf::from).collect(),
        attrs: Default::default(),
    };
    let roots = [PathBuf::from("/x")];
    let is_listing_change = |e: &notify::Event| is_listing_change(e, &roots, 1);

    // 画像が増えた・消えた・名前が変わった → 一覧を作り直す
    assert!(is_listing_change(&event(
        EventKind::Create(CreateKind::File),
        &["/x/new.png"]
    )));
    assert!(is_listing_change(&event(
        EventKind::Remove(RemoveKind::File),
        &["/x/old.jpg"]
    )));
    assert!(is_listing_change(&event(
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
        &["/x/a.png", "/x/b.png"]
    )));
    // 種類が分からない通知は取りこぼすより拾う
    assert!(is_listing_change(&event(EventKind::Any, &["/x/new.png"])));

    // 中身だけの書き換えでは枚数も並び順も変わらない
    assert!(!is_listing_change(&event(
        EventKind::Modify(ModifyKind::Data(DataChange::Content)),
        &["/x/a.png"]
    )));
    // 動画も一覧に並ぶので対象
    assert!(is_listing_change(&event(
        EventKind::Create(CreateKind::File),
        &["/x/new.mp4"]
    )));
    // 画像・動画以外は無関係
    assert!(!is_listing_change(&event(
        EventKind::Create(CreateKind::File),
        &["/x/notes.txt"]
    )));
}

#[test]
fn listing_change_respects_the_depth() {
    use notify::event::{CreateKind, EventKind, RemoveKind};
    let event = |kind, path: &str| notify::Event {
        kind,
        paths: vec![PathBuf::from(path)],
        attrs: Default::default(),
    };
    let roots = [PathBuf::from("/x")];
    let created = |path: &str, depth: usize| {
        is_listing_change(
            &event(EventKind::Create(CreateKind::File), path),
            &roots,
            depth,
        )
    };

    // 3 層目までは拾い、4 層目は捨てる
    assert!(created("/x/a/b/c.png", 3));
    assert!(!created("/x/a/b/c/d.png", 3));
    // 直下だけを見ているときはサブフォルダの中を拾わない
    assert!(!created("/x/a/c.png", 1));

    // 消えたサブフォルダ（拡張子なし）は、中に一覧のファイルがあったかもしれないので拾う
    let removed = |path: &str, depth: usize| {
        is_listing_change(
            &event(EventKind::Remove(RemoveKind::Any), path),
            &roots,
            depth,
        )
    };
    assert!(removed("/x/chapter", 3));
    assert!(removed("/x/a/chapter", 3));
    // 3 層目のフォルダの中身は読まないので無関係。直下だけのときも無関係
    assert!(!removed("/x/a/b/chapter", 3));
    assert!(!removed("/x/chapter", 1));
}

#[test]
fn archive_extension_detection() {
    assert!(is_archive(Path::new("/x/book.CBZ")));
    assert!(is_archive(Path::new("/x/pics.zip")));
    assert!(!is_archive(Path::new("/x/pics.rar")));
    assert!(!is_archive(Path::new("/x/photo.png")));
}

/// 目印の直後から終端記号までを切り出し、その中の "…" を集める
fn quoted_items(text: &str, marker: &str, end: char) -> Vec<String> {
    let rest = text
        .split_once(marker)
        .unwrap_or_else(|| panic!("目印が見つかりません: {marker}"))
        .1;
    let block = &rest[..rest.find(end).expect("リストの終端が見つかりません")];
    block
        .split('"')
        .skip(1)
        .step_by(2)
        .map(String::from)
        .collect()
}

/// 同じく切り出した範囲から、`key: value` の key だけを集める
fn object_keys(text: &str, marker: &str, end: char) -> Vec<String> {
    let rest = text
        .split_once(marker)
        .unwrap_or_else(|| panic!("目印が見つかりません: {marker}"))
        .1;
    let block = &rest[..rest.find(end).expect("リストの終端が見つかりません")];
    block
        .split(',')
        .filter_map(|pair| pair.split_once(':'))
        .map(|(key, _)| key.trim().to_string())
        .collect()
}

fn sorted<I: IntoIterator<Item = S>, S: Into<String>>(items: I) -> Vec<String> {
    let mut v: Vec<String> = items.into_iter().map(Into::into).collect();
    v.sort();
    v
}

#[test]
fn dialog_text_loses_quotes_and_newlines() {
    // OS のコマンドに埋め込むので、クォートを壊す文字が残っていないこと
    let text = sanitize_dialog_text("a'b\"c\\d`e$f\ng\th");
    assert_eq!(text, "a b c d e f g h");
    assert!(!text.contains('\''));
    assert!(!text.contains('"'));
    assert!(!text.contains('\n'));
}

/// 対応拡張子の一覧は lib.rs / main.js / tauri.conf.json にあり、手で揃えるしかない。ずれたらここで落として気付けるようにする
#[test]
fn supported_extensions_stay_in_sync() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("リポジトリのルートが取れません");
    let media_types_js = fs::read_to_string(root.join("src/viewer/media-formats.js"))
        .expect("viewer/media-formats.js を読めません");
    let read_conf = |name: &str| -> serde_json::Value {
        serde_json::from_str(
            &fs::read_to_string(root.join("src-tauri").join(name))
                .unwrap_or_else(|_| panic!("{name} を読めません")),
        )
        .unwrap_or_else(|_| panic!("{name} が JSON として壊れています"))
    };
    let conf = read_conf("tauri.conf.json");

    // fileAssociations から、指定した name の ext を取り出す
    let association = |name: &str| -> Vec<String> {
        conf["bundle"]["fileAssociations"]
            .as_array()
            .expect("fileAssociations がありません")
            .iter()
            .find(|a| a["name"] == name)
            .unwrap_or_else(|| panic!("fileAssociations に {name} がありません"))["ext"]
            .as_array()
            .expect("ext が配列ではありません")
            .iter()
            .map(|e| e.as_str().expect("ext が文字列ではありません").to_string())
            .collect()
    };

    let images = sorted(IMAGE_EXTS.to_vec());
    assert_eq!(
        sorted(quoted_items(
            &media_types_js,
            "const IMAGE_EXT_FILTER = [",
            ']'
        )),
        images,
        "main.js の IMAGE_EXT_FILTER が IMAGE_EXTS とずれています"
    );
    assert_eq!(
        sorted(object_keys(&media_types_js, "const MIME = {", '}')),
        images,
        "main.js の MIME が IMAGE_EXTS とずれています"
    );
    assert_eq!(
        sorted(association("Image")),
        images,
        "tauri.conf.json の fileAssociations が IMAGE_EXTS とずれています"
    );

    let videos = sorted(VIDEO_EXTS.to_vec());
    assert_eq!(
        sorted(quoted_items(
            &media_types_js,
            "const VIDEO_EXT_FILTER = [",
            ']'
        )),
        videos,
        "main.js の VIDEO_EXT_FILTER が VIDEO_EXTS とずれています"
    );
    assert_eq!(
        sorted(association("Video")),
        videos,
        "tauri.conf.json の fileAssociations が VIDEO_EXTS とずれています"
    );
    let audios = sorted(AUDIO_EXTS.to_vec());
    assert_eq!(
        sorted(quoted_items(
            &media_types_js,
            "const AUDIO_EXT_FILTER = [",
            ']'
        )),
        audios,
        "main.js の AUDIO_EXT_FILTER が AUDIO_EXTS とずれています"
    );
    assert_eq!(
        sorted(association("Audio")),
        audios,
        "tauri.conf.json の fileAssociations が AUDIO_EXTS とずれています"
    );

    let archives = sorted(ARCHIVE_EXTS.to_vec());
    assert_eq!(
        sorted(quoted_items(
            &media_types_js,
            "const ARCHIVE_EXT_FILTER = [",
            ']'
        )),
        archives,
        "main.js の ARCHIVE_EXT_FILTER が ARCHIVE_EXTS とずれています"
    );
    // 書庫は開けるが OS には関連付けない（ドラッグ＆ドロップと O キーで開く）。
    // 関連付けは画像・動画・音楽の 3 件だけ
    assert_eq!(
        conf["bundle"]["fileAssociations"]
            .as_array()
            .map(|a| a.len()),
        Some(3),
        "tauri.conf.json の fileAssociations は画像・動画・音楽の 3 件だけにします"
    );
}

/// MP4 の箱を (種類, 中身) に分ける（テストで書き出した fMP4 を確かめる）
fn mp4_boxes(data: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut p = 0;
    while p < data.len() {
        let size = u32::from_be_bytes(data[p..p + 4].try_into().unwrap()) as usize;
        assert!(
            size >= 8 && p + size <= data.len(),
            "箱の大きさが壊れています"
        );
        out.push((
            data[p + 4..p + 8].try_into().unwrap(),
            &data[p + 8..p + size],
        ));
        p += size;
    }
    out
}

fn mp4_child<'a>(data: &'a [u8], kind: &[u8; 4]) -> &'a [u8] {
    mp4_boxes(data)
        .into_iter()
        .find(|(k, _)| k == kind)
        .unwrap_or_else(|| panic!("{} がありません", String::from_utf8_lossy(kind)))
        .1
}

fn be32(data: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(data[at..at + 4].try_into().unwrap())
}

/// テスト用のサンプル（DTS, PTS, 長さ, キーフレームか。時刻はトラックの単位）
#[derive(Debug)]
struct ParsedSample {
    dts: i64,
    pts: i64,
    duration: i64,
    keyframe: bool,
}

/// 断片（moof + mdat）をトラック番号ごとのサンプルに分ける。data_offset と mdat も確かめる
fn parse_fragment(fragment: &[u8]) -> BTreeMap<u32, Vec<ParsedSample>> {
    let boxes = mp4_boxes(fragment);
    assert_eq!(boxes.len(), 2);
    assert_eq!(&boxes[0].0, b"moof");
    assert_eq!(&boxes[1].0, b"mdat");
    let moof_size = boxes[0].1.len() + 8;
    let mut out = BTreeMap::new();
    let mut expected_offset = moof_size + 8;
    for (kind, traf) in mp4_boxes(boxes[0].1) {
        if &kind != b"traf" {
            continue;
        }
        let track = be32(mp4_child(traf, b"tfhd"), 4);
        let tfdt = mp4_child(traf, b"tfdt");
        let mut dts = u64::from_be_bytes(tfdt[4..12].try_into().unwrap()) as i64;
        let trun = mp4_child(traf, b"trun");
        let flags = be32(trun, 0) & 0xFF_FFFF;
        let count = be32(trun, 4) as usize;
        assert_eq!(
            be32(trun, 8) as usize,
            expected_offset,
            "data_offset がずれています"
        );
        let has_cts = flags & 0x800 != 0;
        let stride = if has_cts { 16 } else { 12 };
        let mut samples = Vec::new();
        for i in 0..count {
            let at = 12 + i * stride;
            let duration = be32(trun, at) as i64;
            expected_offset += be32(trun, at + 4) as usize;
            let cts = if has_cts {
                be32(trun, at + 12) as i32 as i64
            } else {
                0
            };
            samples.push(ParsedSample {
                dts,
                pts: dts + cts,
                duration,
                keyframe: be32(trun, at + 8) & 0x0001_0000 == 0,
            });
            dts += duration;
        }
        out.insert(track, samples);
    }
    assert_eq!(
        expected_offset,
        fragment.len(),
        "mdat の大きさがサンプルの合計と違います"
    );
    out
}

fn remux_testdata(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join(name)
}

#[test]
fn mkv_is_remuxed_to_fragmented_mp4() {
    // ffmpeg で作った 3 秒の動画（H.264・B フレームあり・24 fps・1 秒ごとのキーフレーム）。
    // 音声は AC-3（既定）と AAC（日本語）の 2 本
    let (mut stream, info) = MkvStream::open(&remux_testdata("remux.mkv"), None).unwrap();
    assert!(
        info.video_codec.starts_with("avc1.64"),
        "{}",
        info.video_codec
    );
    assert!((info.duration.unwrap() - 3.0).abs() < 0.1);
    assert_eq!(info.audio_tracks.len(), 2);
    assert_eq!(info.audio_tracks[0].codec, "AC-3");
    assert!(info.audio_tracks[0].default);
    assert_eq!(info.audio_tracks[0].codec_string, None);
    assert_eq!(info.audio_tracks[1].language.as_deref(), Some("jpn"));
    // 既定の AC-3 は詰め直せないので、詰め直せる AAC を鳴らす
    assert_eq!(info.audio, Some(1));
    assert_eq!(info.audio_codec.as_deref(), Some("mp4a.40.2"));

    let init = stream.init_segment();
    let top = mp4_boxes(&init);
    assert_eq!(&top[0].0, b"ftyp");
    assert_eq!(&top[1].0, b"moov");
    let moov = mp4_boxes(top[1].1);
    assert_eq!(moov.iter().filter(|(k, _)| k == b"trak").count(), 2);
    assert!(moov.iter().any(|(k, _)| k == b"mvex"));
    let has = |needle: &[u8]| init.windows(needle.len()).any(|w| w == needle);
    assert!(has(b"avc1") && has(b"avcC") && has(b"mp4a") && has(b"esds"));

    let mut video = Vec::new();
    let mut audio_end: Option<i64> = None;
    loop {
        let fragment = stream.read_fragment().unwrap();
        if fragment.is_empty() {
            break;
        }
        let tracks = parse_fragment(&fragment);
        let v = &tracks[&1];
        assert!(v[0].keyframe, "断片はキーフレームから始めます");
        video.extend(tracks[&1].iter().map(|s| (s.dts, s.pts, s.keyframe)));
        // 音声は断片の継ぎ目で途切れも重なりもしない
        let a = &tracks[&2];
        if let Some(end) = audio_end {
            assert_eq!(a[0].dts, end);
        } else {
            assert_eq!(a[0].dts, 0);
        }
        audio_end = Some(a.last().unwrap().dts + 1024);
    }
    assert_eq!(video.len(), 72);
    assert_eq!(video.iter().filter(|v| v.2).count(), 3);
    assert_eq!(video[0].0, 0);
    for pair in video.windows(2) {
        assert!(pair[0].0 < pair[1].0, "DTS が増えていません: {pair:?}");
    }
    for v in &video {
        assert!(v.0 <= v.1, "DTS が PTS より後ろです: {v:?}");
    }
    // 表示時刻はそのまま（並べ替えると 1/24 秒おき。MKV はミリ秒単位なので ±1 ms）
    let mut pts: Vec<i64> = video.iter().map(|v| v.1).collect();
    pts.sort_unstable();
    for (i, p) in pts.iter().enumerate() {
        let exact = i as i64 * 90_000 / 24;
        assert!((p - exact).abs() <= 90, "{i}: {p} != {exact}");
    }
    // 音声は 3 秒ぶん（48 kHz）
    let audio_seconds = audio_end.unwrap() as f64 / 48_000.0;
    assert!((audio_seconds - 3.0).abs() < 0.05, "{audio_seconds}");
}

#[test]
fn mkv_seek_starts_at_previous_keyframe() {
    let (mut stream, _) = MkvStream::open(&remux_testdata("remux.mkv"), Some(1)).unwrap();
    for (target, keyframe) in [(1.5, 1.0), (0.2, 0.0), (2.0, 2.0), (10.0, 2.0)] {
        let found = stream.seek(target).unwrap();
        assert!((found - keyframe).abs() < 0.001, "{target}: {found}");
        let tracks = parse_fragment(&stream.read_fragment().unwrap());
        let first = &tracks[&1][0];
        assert!(first.keyframe);
        assert_eq!(first.pts, (keyframe * 90_000.0) as i64);
        // 音声もシーク先あたりから始まる（ファイルではキーフレームの少し前の音声が後ろに並ぶ）
        let audio = tracks[&2][0].dts as f64 / 48_000.0;
        assert!(audio > keyframe - 0.2 && audio < keyframe + 0.05, "{audio}");
    }
}

#[test]
fn mkv_audio_can_be_chosen_or_left_out() {
    let path = remux_testdata("remux.mkv");
    // 音声なし
    let (mut stream, info) = MkvStream::open(&path, Some(-1)).unwrap();
    assert_eq!(info.audio, None);
    assert_eq!(info.audio_codec, None);
    assert_eq!(
        mp4_boxes(&stream.init_segment())[1]
            .1
            .windows(4)
            .filter(|w| w == b"trak")
            .count(),
        1
    );
    assert!(!parse_fragment(&stream.read_fragment().unwrap()).contains_key(&2));
    // 詰め直せない音声（AC-3）を選んだら音声なし
    let (_, info) = MkvStream::open(&path, Some(0)).unwrap();
    assert_eq!(info.audio, None);
    // 音声の無い MKV
    let (_, info) = MkvStream::open(&remux_testdata("subtitles.mkv"), None).unwrap();
    assert!(info.audio_tracks.is_empty());
    assert_eq!(info.audio, None);
}

#[test]
fn mkv_with_unsupported_video_is_rejected() {
    let err = MkvStream::open(&remux_testdata("unsupported.mkv"), None)
        .err()
        .unwrap();
    assert_eq!(err, "この MKV の映像（MPEG-4 Visual）には対応していません");
}

#[test]
fn decode_times_stay_before_presentation_times() {
    // デコード順 I P B B（24 fps、90 kHz）。並べ替えの深さは 1 フレーム
    let f = 3750;
    let t = video_decode_times(&[0, 3 * f, f, 2 * f], f, None, f);
    // 頭の負の DTS は 0 から詰める
    assert_eq!(t.dts, vec![0, f / 2, f, 2 * f]);
    assert_eq!(t.durations, vec![f / 2, f / 2, f, f]);
    // ファイルの途中（負にならない）なら、昇順の PTS を深さだけ前へずらす。最後の長さは次の DTS まで
    let t = video_decode_times(&[40 * f, 43 * f, 41 * f, 42 * f], f, Some(43 * f), f);
    assert_eq!(t.dts, vec![39 * f, 40 * f, 41 * f, 42 * f]);
    assert_eq!(t.durations, vec![f, f, f, f]);
    assert_eq!(reorder_depth(&[0, 3 * f, f, 2 * f]), f);
    assert_eq!(reorder_depth(&[0, f, 2 * f]), 0);
}

#[test]
fn laced_frames_are_split() {
    // Xiph: 3 フレーム（2, 256, 1 バイト）
    let mut xiph = vec![2, 2, 255, 1];
    xiph.extend([1, 1]);
    xiph.extend(vec![2; 256]);
    xiph.push(3);
    let frames = split_laced_frames(&xiph, 1).unwrap();
    assert_eq!(
        frames.iter().map(Vec::len).collect::<Vec<_>>(),
        vec![2, 256, 1]
    );
    assert_eq!(frames[2], vec![3]);
    // 固定長: 2 フレーム × 3 バイト
    let frames = split_laced_frames(&[1, 1, 2, 3, 4, 5, 6], 2).unwrap();
    assert_eq!(frames, vec![vec![1, 2, 3], vec![4, 5, 6]]);
    // EBML: 3 フレーム（4, 2, 1 バイト）。2 つ目は 1 つ目との差 -2（1 バイトの符号付き = 0x80 | (63 - 2)）
    let mut ebml = vec![2, 0x84, 0x80 | 61];
    ebml.extend([9; 7]);
    let frames = split_laced_frames(&ebml, 3).unwrap();
    assert_eq!(
        frames.iter().map(Vec::len).collect::<Vec<_>>(),
        vec![4, 2, 1]
    );
    // 足りないものは壊れているとみなす
    assert_eq!(split_laced_frames(&[1, 200, 1], 1), None);
}

#[test]
fn codec_strings_follow_the_mp4_rules() {
    assert_eq!(
        avc_codec_string(&[1, 0x64, 0x00, 0x1F, 0xFF, 0xE1, 0]).as_deref(),
        Some("avc1.64001F")
    );
    let mut hvcc = vec![0u8; 23];
    hvcc[0] = 1;
    hvcc[1] = 0x01; // Main
    hvcc[2] = 0x60;
    hvcc[6] = 0x90;
    hvcc[12] = 93;
    assert_eq!(hevc_codec_string(&hvcc).as_deref(), Some("hvc1.1.6.L93.90"));
    hvcc[1] = 0x22; // High tier、Main 10
    hvcc[2] = 0x20;
    hvcc[6] = 0xB0;
    hvcc[12] = 120;
    assert_eq!(
        hevc_codec_string(&hvcc).as_deref(),
        Some("hvc1.2.4.H120.B0")
    );
    assert_eq!(hevc_codec_string(&[1, 2, 3]), None);

    // AAC-LC 48 kHz ステレオ / 960 サンプルのフレーム / HE-AAC
    assert_eq!(parse_aac_config(&[0x11, 0x90]), Some((2, 48000, 1024)));
    assert_eq!(parse_aac_config(&[0x11, 0x94]), Some((2, 48000, 960)));
    assert_eq!(
        parse_aac_config(&[0x2B, 0x92, 0x08, 0x00]),
        Some((5, 22050, 1024))
    );
    assert_eq!(
        legacy_aac_config("A_AAC/MPEG4/LC", 48000, 2),
        Some(vec![0x11, 0x90])
    );
    assert_eq!(
        legacy_aac_config("A_AAC/MPEG2/LC/SBR", 44100, 2),
        Some(vec![0x13, 0x90])
    );

    // Opus: 20 ms（CELT）1 フレーム / 10 ms × 2 / コード 3 で 60 ms ずつ 2 フレーム
    assert_eq!(opus_packet_samples(&[0xF8]), Some(960));
    assert_eq!(opus_packet_samples(&[(14 << 3) | 1]), Some(960));
    assert_eq!(opus_packet_samples(&[(3 << 3) | 3, 2]), Some(5760));
    let mut head = b"OpusHead".to_vec();
    head.extend([1, 2, 0x38, 0x01, 0x80, 0xBB, 0, 0, 0, 0, 0]);
    assert_eq!(
        opus_dops(&head),
        Some(vec![0, 2, 0x01, 0x38, 0, 0, 0xBB, 0x80, 0, 0, 0])
    );
}

#[test]
fn mp4_descriptor_lengths_use_seven_bit_groups() {
    assert_eq!(mp4_descriptor(5, &[1, 2]), vec![5, 2, 1, 2]);
    let long = mp4_descriptor(5, &[0; 200]);
    assert_eq!(&long[..3], &[5, 0x81, 0x48]);
    assert_eq!(long.len(), 203);
}

#[test]
fn mkv_subtitles_are_read_from_srt_and_ass_tracks() {
    // ffmpeg で作った 2 秒の動画。字幕は SRT（日本語・既定）と ASS（英語）の 2 本
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/subtitles.mkv");
    let tracks = read_mkv_subtitles(&path).unwrap();
    assert_eq!(tracks.len(), 2);

    let srt = &tracks[0];
    assert_eq!(srt.name.as_deref(), Some("日本語"));
    assert_eq!(srt.language.as_deref(), Some("jpn"));
    assert!(srt.default);
    let texts: Vec<&str> = srt.cues.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, vec!["こんにちは\n<i>二行目</i>", "上に出す"]);
    assert!((srt.cues[0].start - 0.5).abs() < 0.01);
    assert!((srt.cues[0].end - 1.2).abs() < 0.01);

    let ass = &tracks[1];
    assert_eq!(ass.language.as_deref(), Some("eng"));
    assert!(!ass.default);
    let texts: Vec<&str> = ass.cues.iter().map(|c| c.text.as_str()).collect();
    // 上書きタグは外し、描画命令だけの行は捨て、記号は文字参照にする
    assert_eq!(texts, vec!["Hello, world\nline two", "a &lt; b"]);
    assert!((ass.cues[0].start - 0.2).abs() < 0.01);
    assert!((ass.cues[0].end - 1.0).abs() < 0.01);
}

#[test]
fn subtitles_are_only_read_from_mkv() {
    // MKV 以外は読まずに空を返す
    assert!(video_subtitles("/x/clip.mp4".into()).unwrap().is_empty());
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/subtitles.mkv");
    assert_eq!(
        video_subtitles(path.to_string_lossy().into_owned())
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn ass_text_drops_override_tags_and_drawings() {
    assert_eq!(clean_ass_text(r"{\i1}a{\i0}\Nb\hc"), "a\nb\u{a0}c");
    assert_eq!(clean_ass_text(r"{\pos(10,20)}x"), "x");
    assert_eq!(clean_ass_text(r"a{\p1}m 0 0 l 1 1{\p0}b"), "ab");
    assert_eq!(
        subtitle_text(b"0,0,Default,,0,0,0,,{\\b1}Hi, there", SubtitleFormat::Ass).as_deref(),
        Some("Hi, there")
    );
    assert_eq!(subtitle_text(b"{\\an8}  \r\n", SubtitleFormat::Srt), None);
}

#[test]
fn subtitle_blocks_are_decompressed() {
    use std::io::Write;
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(b"hello").unwrap();
    let zipped = z.finish().unwrap();
    assert_eq!(
        decode_subtitle_block(zipped, &[BlockDecode::Zlib]).as_deref(),
        Some(&b"hello"[..])
    );
    assert_eq!(
        decode_subtitle_block(b"llo".to_vec(), &[BlockDecode::Strip(b"he".to_vec())]).as_deref(),
        Some(&b"hello"[..])
    );
    assert_eq!(
        decode_subtitle_block(b"bad".to_vec(), &[BlockDecode::Zlib]),
        None
    );
}

#[test]
fn common_prefix_strips_only_a_shared_folder() {
    let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    // 余計に一層挟まれている → その一層を吸収
    assert_eq!(
        common_dir_prefix(&v(&["book/001.png", "book/002.png"])),
        "book/"
    );
    assert_eq!(common_dir_prefix(&v(&["a/b/1.png", "a/b/2.png"])), "a/b/");
    // 章分けされている → 共通部分までしか削らない
    assert_eq!(common_dir_prefix(&v(&["a/b/1.png", "a/c/2.png"])), "a/");
    assert_eq!(common_dir_prefix(&v(&["ch1/1.png", "ch2/1.png"])), "");
    // 名前が前方一致するだけの別フォルダを誤って削らない
    assert_eq!(common_dir_prefix(&v(&["ch1/1.png", "ch10/1.png"])), "");
    // 書庫直下に画像が並んでいる → 削らない
    assert_eq!(common_dir_prefix(&v(&["001.png", "002.png"])), "");
    assert_eq!(common_dir_prefix(&[]), "");
}

/// 画像3枚とテキスト・動画各1件を含む zip を作り、一覧と単体取り出しを確認する
#[test]
fn archive_listing_and_single_entry_read() {
    use std::io::Write;
    let dir = std::env::temp_dir().join(format!("sview-test-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.zip");
    {
        let mut w = zip::ZipWriter::new(File::create(&path).unwrap());
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, body) in [
            ("b/img10.png", "ten"),
            ("b/img2.png", "two"),
            ("readme.txt", "nope"),
            // 書庫の中の動画は一覧に出さない（丸ごとメモリに読む方式なので）
            ("b/clip.mp4", "video"),
            // 4 層目は読まない
            ("b/c/d/img1.png", "deep"),
        ] {
            w.start_file(name, opts).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        // 圧縮済みエントリも展開できることを確認するため 1 件だけ deflate で入れる
        let deflated: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        w.start_file("b/img3.png", deflated).unwrap();
        w.write_all(&b"three".repeat(500)).unwrap();
        w.finish().unwrap();
    }

    let cache = ArchiveCache(Mutex::new(None));
    let list = list_archive_images(&path, &cache).unwrap();
    assert_eq!(list.images, vec!["b/img2.png", "b/img3.png", "b/img10.png"]);
    assert_eq!(list.kind, Some(MediaKind::Image));
    assert_eq!(
        list.archive.as_deref(),
        Some(path.to_string_lossy().as_ref())
    );
    // 画像はすべて b/ の下なので、その一層は表示名から取り除かれる
    assert_eq!(list.prefix, "b/");

    let bytes = read_archive_entry(&path, "b/img2.png", &cache).unwrap();
    assert_eq!(bytes, b"two");
    let bytes = read_archive_entry(&path, "b/img3.png", &cache).unwrap();
    assert_eq!(bytes, b"three".repeat(500));
    // 存在しないエントリ
    assert!(read_archive_entry(&path, "b/none.png", &cache).is_err());
    // 画像以外は取り出せない
    assert!(read_archive_entry(&path, "readme.txt", &cache).is_err());
    assert!(read_archive_entry(&path, "b/clip.mp4", &cache).is_err());

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn av1_codec_string_and_helpers() {
    // 8bit Main / 10bit High / 12bit Professional
    assert_eq!(
        av1_codec_string(&[0x81, 0x04, 0x0C, 0x00]).as_deref(),
        Some("av01.0.04M.08")
    );
    assert_eq!(
        av1_codec_string(&[0x81, 0x0D, 0xCC, 0x00]).as_deref(),
        Some("av01.0.13H.10")
    );
    assert_eq!(
        av1_codec_string(&[0x81, 0x48, 0x6C, 0x00]).as_deref(),
        Some("av01.2.08M.12")
    );
    assert_eq!(av1_codec_string(&[0x80, 0x04, 0x0C, 0x00]), None);
    assert_eq!(av1_codec_string(&[0x81, 0x04, 0x0C]), None);
    assert_eq!(av1_codec_string(&[0x81, 0x60, 0x0C, 0x00]), None);

    // SPS / PPS から avcC を組み立てて codecs 文字列へ戻せる
    let avcc = build_avcc_from_sps_pps(&[0x67, 0x64, 0x00, 0x1F, 0xAC], &[0x68, 0xEE]).unwrap();
    assert_eq!(avc_codec_string(&avcc).as_deref(), Some("avc1.64001F"));
    assert_eq!(build_avcc_from_sps_pps(&[0x67, 0x64], &[0x68]), None);
    assert_eq!(
        build_avcc_from_sps_pps(&[0x67, 0x64, 0x00, 0x1F], &[]),
        None
    );

    // SDR はそのまま、HDR (PQ / HLG) は 8bit H.264 向けに BT.709 へ落とす、未指定は None
    let bt709 = ColorInfo {
        primaries: 1,
        transfer: 1,
        matrix: 1,
        full_range: false,
    };
    assert_eq!(bt709.for_sdr_h264(), Some(bt709));
    let hdr_pq = ColorInfo {
        primaries: 9,
        transfer: 16,
        matrix: 9,
        full_range: false,
    };
    assert_eq!(hdr_pq.for_sdr_h264(), Some(bt709));
    let hdr_hlg = ColorInfo {
        primaries: 9,
        transfer: 18,
        matrix: 9,
        full_range: true,
    };
    assert_eq!(
        hdr_hlg.for_sdr_h264(),
        Some(ColorInfo {
            primaries: 1,
            transfer: 1,
            matrix: 1,
            full_range: true,
        })
    );
    let unspecified = ColorInfo {
        primaries: 2,
        transfer: 2,
        matrix: 2,
        full_range: false,
    };
    assert_eq!(unspecified.for_sdr_h264(), None);

    // ビットレートの上下限と 1080p の目安
    assert_eq!(target_h264_bitrate(64, 48, 24.0), 2_000_000);
    let fhd = target_h264_bitrate(1920, 1080, 30.0);
    assert!((10_000_000..=15_000_000).contains(&fhd), "{fhd}");
    assert_eq!(target_h264_bitrate(7680, 4320, 60.0), 60_000_000);
}

#[test]
fn nv12_conversion_handles_bit_depths_and_layouts() {
    // 1. 8bit I420（ストライド余白あり、dst が src より広いときのパディングも含む）
    let y_src = [10, 20, 30, 40, 99, 99, 50, 60, 70, 80, 99, 99];
    let u_src = [100, 110, 99];
    let v_src = [200, 210, 99];
    let frame = DecodedFrame {
        width: 4,
        height: 2,
        bpc: 8,
        layout: PixelLayout::I420,
        y: PlaneView::from_slice(&y_src, 6, 4, 2),
        u: Some(PlaneView::from_slice(&u_src, 3, 2, 1)),
        v: Some(PlaneView::from_slice(&v_src, 3, 2, 1)),
        pts: 0,
        color: None,
    };
    let mut dst_y = vec![0xFFu8; 8 * 4];
    let mut dst_uv = vec![0xFFu8; 8 * 2];
    copy_frame_to_nv12(&frame, 6, 4, &mut dst_y, 8, &mut dst_uv, 8);
    assert_eq!(&dst_y[0..6], &[10, 20, 30, 40, 16, 16]);
    assert_eq!(&dst_y[8..14], &[50, 60, 70, 80, 16, 16]);
    assert_eq!(&dst_y[16..22], &[16; 6]);
    assert_eq!(&dst_uv[0..6], &[100, 200, 110, 210, 128, 128]);
    assert_eq!(&dst_uv[8..14], &[128; 6]);

    // 2. 10bit I420（丸め付きダウンシフト）
    let mut y10 = Vec::new();
    for val in [0u16, 400, 1020, 1023, 200, 300, 500, 600] {
        y10.extend_from_slice(&val.to_le_bytes());
    }
    let mut u10 = Vec::new();
    for val in [512u16, 1022] {
        u10.extend_from_slice(&val.to_le_bytes());
    }
    let mut v10 = Vec::new();
    for val in [256u16, 768] {
        v10.extend_from_slice(&val.to_le_bytes());
    }
    let frame10 = DecodedFrame {
        width: 4,
        height: 2,
        bpc: 10,
        layout: PixelLayout::I420,
        y: PlaneView::from_slice(&y10, 8, 8, 2),
        u: Some(PlaneView::from_slice(&u10, 4, 4, 1)),
        v: Some(PlaneView::from_slice(&v10, 4, 4, 1)),
        pts: 0,
        color: None,
    };
    let mut dst_y = vec![0u8; 8];
    let mut dst_uv = vec![0u8; 4];
    copy_frame_to_nv12(&frame10, 4, 2, &mut dst_y, 4, &mut dst_uv, 4);
    assert_eq!(dst_y, vec![0, 100, 255, 255, 50, 75, 125, 150]);
    assert_eq!(dst_uv, vec![128, 64, 255, 192]);

    // 3. 12bit I420
    let mut y12 = Vec::new();
    for val in [0u16, 2048, 4095, 1024] {
        y12.extend_from_slice(&val.to_le_bytes());
    }
    let u12 = 2048u16.to_le_bytes();
    let v12 = 4095u16.to_le_bytes();
    let frame12 = DecodedFrame {
        width: 2,
        height: 2,
        bpc: 12,
        layout: PixelLayout::I420,
        y: PlaneView::from_slice(&y12, 4, 4, 2),
        u: Some(PlaneView::from_slice(&u12, 2, 2, 1)),
        v: Some(PlaneView::from_slice(&v12, 2, 2, 1)),
        pts: 0,
        color: None,
    };
    let mut dst_y = vec![0u8; 4];
    let mut dst_uv = vec![0u8; 2];
    copy_frame_to_nv12(&frame12, 2, 2, &mut dst_y, 2, &mut dst_uv, 2);
    assert_eq!(dst_y, vec![0, 128, 255, 64]);
    assert_eq!(dst_uv, vec![128, 255]);

    // 4. 8bit I422（縦 2 行おきに色差を間引く）
    let y422 = [1u8; 8];
    let u422 = [11, 22, 33, 44];
    let v422 = [55, 66, 77, 88];
    let frame422 = DecodedFrame {
        width: 4,
        height: 2,
        bpc: 8,
        layout: PixelLayout::I422,
        y: PlaneView::from_slice(&y422, 4, 4, 2),
        u: Some(PlaneView::from_slice(&u422, 2, 2, 2)),
        v: Some(PlaneView::from_slice(&v422, 2, 2, 2)),
        pts: 0,
        color: None,
    };
    let mut dst_y = vec![0u8; 8];
    let mut dst_uv = vec![0u8; 4];
    copy_frame_to_nv12(&frame422, 4, 2, &mut dst_y, 4, &mut dst_uv, 4);
    assert_eq!(dst_uv, vec![11, 55, 22, 66]);

    // 5. 8bit I444（横・縦とも 2 画素おきに間引く）
    let y444 = [2u8; 8];
    let u444 = [10, 11, 20, 21, 30, 31, 40, 41];
    let v444 = [50, 51, 60, 61, 70, 71, 80, 81];
    let frame444 = DecodedFrame {
        width: 4,
        height: 2,
        bpc: 8,
        layout: PixelLayout::I444,
        y: PlaneView::from_slice(&y444, 4, 4, 2),
        u: Some(PlaneView::from_slice(&u444, 4, 4, 2)),
        v: Some(PlaneView::from_slice(&v444, 4, 4, 2)),
        pts: 0,
        color: None,
    };
    let mut dst_y = vec![0u8; 8];
    let mut dst_uv = vec![0u8; 4];
    copy_frame_to_nv12(&frame444, 4, 2, &mut dst_y, 4, &mut dst_uv, 4);
    assert_eq!(dst_uv, vec![10, 50, 20, 60]);

    // 6. 8bit I400（モノクロ: UV を無彩色 128 で埋め、フルレンジ時は Y 余白を 0 にする）
    let y400 = [77, 88, 99, 111];
    let frame400 = DecodedFrame {
        width: 2,
        height: 2,
        bpc: 8,
        layout: PixelLayout::I400,
        y: PlaneView::from_slice(&y400, 2, 2, 2),
        u: None,
        v: None,
        pts: 0,
        color: Some(ColorInfo {
            primaries: 1,
            transfer: 1,
            matrix: 1,
            full_range: true,
        }),
    };
    let mut dst_y = vec![0xFFu8; 16];
    let mut dst_uv = vec![0u8; 8];
    copy_frame_to_nv12(&frame400, 4, 4, &mut dst_y, 4, &mut dst_uv, 4);
    assert_eq!(&dst_y[0..4], &[77, 88, 0, 0]);
    assert_eq!(&dst_y[4..8], &[99, 111, 0, 0]);
    assert_eq!(&dst_y[8..16], &[0; 8]);
    assert_eq!(dst_uv, vec![128; 8]);
}

#[test]
fn av1_video_detection_distinguishes_codecs_and_containers() {
    let mp4 = remux_testdata("av1.mp4");
    let webm = remux_testdata("av1.webm");
    let h264 = remux_testdata("remux.mkv");
    let mpeg4 = remux_testdata("unsupported.mkv");

    assert!(is_mp4_av1(&mp4));
    assert!(is_av1_video(&mp4));

    assert!(!is_mp4_av1(&webm));
    assert!(is_av1_video(&webm));

    assert!(!is_mp4_av1(&h264));
    assert!(!is_av1_video(&h264));
    assert!(!is_av1_video(&mpeg4));
}

#[test]
fn av1_mp4_and_webm_remux_and_fragmented_mp4_roundtrip() {
    let mut fmp4_bytes = Vec::new();
    for (name, expected_audio_codec, sample_step) in [
        ("av1.mp4", "mp4a.40.2", 1024i64),
        ("av1.webm", "opus", 960i64),
    ] {
        let (mut stream, info) =
            MkvStream::open_with_options(&remux_testdata(name), None, Some(false)).unwrap();
        assert!(!info.transcoded);
        assert!(
            info.video_codec.starts_with("av01."),
            "{name}: {}",
            info.video_codec
        );
        assert_eq!(info.audio_codec.as_deref(), Some(expected_audio_codec));
        assert!((info.duration.unwrap() - 2.0).abs() < 0.15, "{name}");

        let init = stream.init_segment();
        let has = |needle: &[u8]| init.windows(needle.len()).any(|w| w == needle);
        assert!(
            has(b"av01") && has(b"av1C"),
            "{name}: av01/av1C がありません"
        );
        if name == "av1.mp4" {
            fmp4_bytes.extend_from_slice(&init);
        }

        let mut video = Vec::new();
        let mut audio_end: Option<i64> = None;
        loop {
            let fragment = stream.read_fragment().unwrap();
            if fragment.is_empty() {
                break;
            }
            if name == "av1.mp4" {
                fmp4_bytes.extend_from_slice(&fragment);
            }
            let tracks = parse_fragment(&fragment);
            let v = &tracks[&1];
            assert!(v[0].keyframe, "{name}: 断片はキーフレームから始めます");
            video.extend(v.iter().map(|s| (s.dts, s.pts, s.keyframe)));
            if let Some(a) = tracks.get(&2) {
                if let Some(end) = audio_end {
                    assert_eq!(a[0].dts, end, "{name}: 音声の継ぎ目が途切れています");
                } else {
                    assert!(
                        a[0].dts <= 2400,
                        "{name}: 音声の開始が遅れています {}",
                        a[0].dts
                    );
                }
                audio_end = Some(a.last().unwrap().dts + sample_step);
            }
        }
        assert_eq!(video.len(), 48, "{name}: 映像フレーム数");
        let audio_sec = audio_end.unwrap() as f64 / 48_000.0;
        assert!((audio_sec - 2.0).abs() < 0.12, "{name}: {audio_sec}");
    }

    // 書き出した fMP4（moov + mvex + moof/mdat）をそのまま fragmented MP4 として読み戻せること
    let tmp = std::env::temp_dir().join(format!("sview-fmp4-test-{}.mp4", std::process::id()));
    fs::write(&tmp, &fmp4_bytes).unwrap();
    assert!(is_mp4_av1(&tmp));
    assert!(is_av1_video(&tmp));
    let (mut stream, info) = MkvStream::open_with_options(&tmp, None, Some(false)).unwrap();
    assert!(info.video_codec.starts_with("av01."));
    let mut frames = 0usize;
    let mut audio_samples = 0usize;
    loop {
        let fragment = stream.read_fragment().unwrap();
        if fragment.is_empty() {
            break;
        }
        let tracks = parse_fragment(&fragment);
        frames += tracks[&1].len();
        audio_samples += tracks.get(&2).map_or(0, Vec::len);
    }
    assert_eq!(frames, 48);
    assert!(audio_samples > 80, "{audio_samples}");
    fs::remove_file(&tmp).ok();
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn av1_transcode_decodes_real_frames_and_keeps_av_sync() {
    #[cfg(target_os = "linux")]
    struct MockH264Encoder {
        width: usize,
        height: usize,
        avcc: Vec<u8>,
        pending: Vec<EncodedFrame>,
    }

    #[cfg(target_os = "linux")]
    impl VideoEncoder for MockH264Encoder {
        fn encode(&mut self, frame: &DecodedFrame<'_>, force_keyframe: bool) -> Result<(), String> {
            assert_eq!(frame.width, self.width);
            assert_eq!(frame.height, self.height);
            // 実際に rav1d がデコードした画素を NV12 変換して中身を確かめる
            let mut dst_y = vec![0u8; self.width * self.height];
            let mut dst_uv = vec![0u8; self.width * self.height.div_ceil(2)];
            copy_frame_to_nv12(
                frame,
                self.width,
                self.height,
                &mut dst_y,
                self.width,
                &mut dst_uv,
                self.width,
            );
            assert!(
                dst_y.iter().any(|&v| v != 0),
                "復号された Y プレーンが空です"
            );
            let nal_type = if force_keyframe { 0x65 } else { 0x41 };
            self.pending.push(EncodedFrame {
                data: vec![0, 0, 0, 2, nal_type, 0x88],
                pts: frame.pts,
                keyframe: force_keyframe,
            });
            Ok(())
        }

        fn flush(&mut self) -> Result<Vec<EncodedFrame>, String> {
            Ok(std::mem::take(&mut self.pending))
        }

        fn avcc(&self) -> Option<Vec<u8>> {
            Some(self.avcc.clone())
        }
    }

    let open_transcode = |path: &Path| -> Result<(MkvStream, MkvOpened), String> {
        #[cfg(target_os = "macos")]
        {
            MkvStream::open_with_options(path, None, Some(true))
        }
        #[cfg(target_os = "linux")]
        {
            let factory: EncoderFactory = Box::new(|w, h, _fps, _color| {
                let avcc = build_avcc_from_sps_pps(&[0x67, 0x64, 0x00, 0x1F, 0xAC], &[0x68, 0xEE])
                    .unwrap();
                Ok(Box::new(MockH264Encoder {
                    width: w as usize,
                    height: h as usize,
                    avcc,
                    pending: Vec::new(),
                }))
            });
            MkvStream::open_with_test_encoder(path, None, factory)
        }
    };

    for (name, sample_step) in [("av1.mp4", 1024i64), ("av1.webm", 960i64)] {
        let path = remux_testdata(name);
        let (mut stream, info) = open_transcode(&path).unwrap();
        assert!(info.transcoded, "{name}: 変換モードになっていること");
        assert!(!info.can_transcode);
        assert!(
            info.video_codec.starts_with("avc1."),
            "{name}: {}",
            info.video_codec
        );

        let init = stream.init_segment();
        let has = |needle: &[u8]| init.windows(needle.len()).any(|w| w == needle);
        assert!(
            has(b"avc1") && has(b"avcC"),
            "{name}: avc1/avcC がありません"
        );
        assert!(!has(b"av01"), "{name}: 変換後に av01 が残っています");

        // 1. 先頭から末尾までの再生（全 48 フレーム・B フレームなし・映像も音声も断片境界 0 ティック連続）
        let mut video = Vec::new();
        let mut video_end: Option<i64> = None;
        let mut audio_end: Option<i64> = None;
        let mut fragments = 0usize;
        let mut first_frag_frames = 0usize;
        loop {
            let fragment = stream.read_fragment().unwrap();
            if fragment.is_empty() {
                break;
            }
            fragments += 1;
            let tracks = parse_fragment(&fragment);
            let v = &tracks[&1];
            if fragments == 1 {
                first_frag_frames = v.len();
            }
            assert!(v[0].keyframe, "{name}: 断片の先頭は必ずキーフレーム");
            if let Some(end) = video_end {
                assert_eq!(v[0].dts, end, "{name}: 映像断片の継ぎ目に隙間があります");
            }
            for s in v {
                assert_eq!(s.dts, s.pts, "{name}: B フレームなし (DTS == PTS) を保つ");
            }
            let v_last = v.last().unwrap();
            video_end = Some(v_last.dts + v_last.duration);
            video.extend(v.iter().map(|s| s.pts));
            if let Some(a) = tracks.get(&2) {
                if let Some(end) = audio_end {
                    assert_eq!(a[0].dts, end, "{name}: 音声の継ぎ目が途切れています");
                } else {
                    assert!(a[0].dts <= 2400, "{name}: 音声開始 {}", a[0].dts);
                }
                audio_end = Some(a.last().unwrap().dts + sample_step);
            }
        }
        assert!(fragments >= 3, "{name}: 初回短断片 + 後続断片に分かれる");
        assert!(
            (5..=8).contains(&first_frag_frames),
            "{name}: 初回断片は 0.25 秒前後で素早く返す: {first_frag_frames}"
        );
        assert_eq!(
            video.len(),
            48,
            "{name}: 全 48 フレームが復号・出力されること"
        );
        for pair in video.windows(2) {
            assert!(
                pair[0] < pair[1],
                "{name}: PTS が単調増加していません: {pair:?}"
            );
        }
        for (i, &p) in video.iter().enumerate() {
            let exact = i as i64 * 90_000 / 24;
            assert!((p - exact).abs() <= 90, "{name}[{i}]: {p} != {exact}");
        }
        let audio_sec = audio_end.unwrap() as f64 / 48_000.0;
        assert!(
            (audio_sec - 2.0).abs() < 0.12,
            "{name}: 音声長さ {audio_sec}"
        );

        // 2. GOP 途中（1.25 秒）へのシーク: 先頭断片が 1.25 秒を覆い、1 秒後以降の断片境界も隙間 0 で続く
        let sought = stream.seek(1.25).unwrap();
        assert!(
            (1.19..=1.25).contains(&sought),
            "{name}: シーク戻り値 {sought}"
        );
        let mut seek_video = Vec::new();
        let mut seek_video_end: Option<i64> = None;
        let mut seek_audio_start: Option<f64> = None;
        let mut seek_audio_end: Option<i64> = None;
        loop {
            let fragment = stream.read_fragment().unwrap();
            if fragment.is_empty() {
                break;
            }
            let tracks = parse_fragment(&fragment);
            let v = &tracks[&1];
            assert!(v[0].keyframe);
            if let Some(end) = seek_video_end {
                assert_eq!(
                    v[0].dts, end,
                    "{name}: シーク後の映像断片の継ぎ目に隙間があります"
                );
            }
            let v_last = v.last().unwrap();
            seek_video_end = Some(v_last.dts + v_last.duration);
            seek_video.extend(v.iter().map(|s| s.pts));
            if let Some(a) = tracks.get(&2) {
                if seek_audio_start.is_none() {
                    seek_audio_start = Some(a[0].dts as f64 / 48_000.0);
                }
                if let Some(end) = seek_audio_end {
                    assert_eq!(a[0].dts, end, "{name}: シーク後の音声が途切れています");
                }
                seek_audio_end = Some(a.last().unwrap().dts + sample_step);
            }
        }
        let first_sec = seek_video[0] as f64 / 90_000.0;
        assert!(
            (1.19..=1.25).contains(&first_sec),
            "{name}: シーク先頭映像({first_sec})は目標時刻 1.25 秒以下で覆うこと"
        );
        let a_start = seek_audio_start.unwrap();
        assert!(
            a_start <= first_sec + 0.01 && first_sec - a_start <= 0.05,
            "{name}: シーク直後の映像({first_sec})と音声({a_start})がずれています"
        );
        assert!(
            (18..=21).contains(&seek_video.len()),
            "{name}: 1.20〜2.0 秒のフレーム数 {}",
            seek_video.len()
        );

        // 3. 巻き戻しシーク（0.55 秒）でも継ぎ目なく再生できる
        let back = stream.seek(0.55).unwrap();
        assert!((0.49..=0.55).contains(&back), "{name}: {back}");
        let back_frag = stream.read_fragment().unwrap();
        let back_tracks = parse_fragment(&back_frag);
        let back_first_sec = back_tracks[&1][0].pts as f64 / 90_000.0;
        assert!(
            (0.49..=0.55).contains(&back_first_sec),
            "{name}: 巻き戻し先頭 {back_first_sec}"
        );

        // 4. 末尾以降へのシークでも空にならず最後のキーフレームから再生できる
        let past_end = stream.seek(10.0).unwrap();
        assert!((past_end - 1.0).abs() < 0.05, "{name}: {past_end}");
        let tail_frag = stream.read_fragment().unwrap();
        assert!(
            !tail_frag.is_empty(),
            "{name}: 末尾以降のシークで断片が空です"
        );
        let tail_tracks = parse_fragment(&tail_frag);
        assert!(tail_tracks[&1][0].keyframe);

        // 5. シーク連打の中断フラグで read_fragment が即座に打ち切られ、次の seek で復帰できる
        stream.seek(0.5).unwrap();
        stream
            .abort_flag()
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(stream.read_fragment().unwrap().is_empty());
        stream.seek(0.5).unwrap();
        assert!(!stream.read_fragment().unwrap().is_empty());
    }

    // 奇数解像度の偶数丸めと、5K / 8K 解像度の 4K 以下縮小
    assert_eq!(h264_output_dimensions(853, 480), (854, 480));
    assert_eq!(h264_output_dimensions(1920, 1079), (1920, 1080));
    assert_eq!(h264_output_dimensions(7680, 4320), (4096, 2304));
    assert_eq!(h264_output_dimensions(3840, 2160), (3840, 2160));

    // NV12 の縮小コピー（8K → 4K 相当）が境界外アクセスせず書き込める
    let big_y = vec![200u8; 8 * 6];
    let big_u = vec![90u8; 4 * 3];
    let big_v = vec![150u8; 4 * 3];
    let frame = DecodedFrame {
        width: 8,
        height: 6,
        bpc: 8,
        layout: PixelLayout::I420,
        y: PlaneView::from_slice(&big_y, 8, 8, 6),
        u: Some(PlaneView::from_slice(&big_u, 4, 4, 3)),
        v: Some(PlaneView::from_slice(&big_v, 4, 4, 3)),
        pts: 0,
        color: None,
    };
    let mut dst_y = vec![0u8; 4 * 4];
    let mut dst_uv = vec![0u8; 4 * 2];
    copy_frame_to_nv12(&frame, 4, 4, &mut dst_y, 4, &mut dst_uv, 4);
    assert!(dst_y.iter().all(|&v| v == 200));
    assert_eq!(&dst_uv[..4], &[90, 150, 90, 150]);
}

#[test]
fn vp9_codec_helpers_and_frame_header_parsing() {
    // vpcC（8 バイト）から codecs 文字列を作る
    assert_eq!(
        vp9_codec_string(&[0, 41, 0x80, 1, 1, 1, 0, 0]).as_deref(),
        Some("vp09.00.41.08")
    );
    assert_eq!(
        vp9_codec_string(&[2, 51, 0xA2, 9, 16, 9, 0, 0]).as_deref(),
        Some("vp09.02.51.10")
    );
    assert_eq!(vp9_codec_string(&[0, 41, 0x80]), None);
    assert_eq!(vp9_codec_string(&[4, 41, 0x80, 2, 2, 2, 0, 0]), None);
    // 仕様外のレベル値（25 など）は弾く
    assert_eq!(vp9_codec_string(&[0, 25, 0x80, 1, 1, 1, 0, 0]), None);

    // Matroska の V_VP9 CodecPrivate（TLV）: 仕様外のレベル 25 は無視してフォールバックさせる
    let private = [1, 1, 2, 2, 1, 50, 3, 1, 10, 4, 1, 1];
    assert_eq!(
        parse_mkv_vp9_private(&private),
        (Some(2), Some(50), Some(10), Some(1))
    );
    assert_eq!(parse_mkv_vp9_private(&[2, 1, 25]).1, None);

    // 解像度・フレームレートからのレベル算出（WebKit の上限に合わせ推定値は 60 まで）
    assert_eq!(vp9_level_for(64, 64, 24.0), 10);
    assert_eq!(vp9_level_for(1920, 1080, 30.0), 40);
    assert_eq!(vp9_level_for(1920, 1080, 60.0), 41);
    assert_eq!(vp9_level_for(3840, 2160, 30.0), 50);
    assert_eq!(vp9_level_for(3840, 2160, 60.0), 51);
    assert_eq!(vp9_level_for(3840, 2160, 1000.0), 52);
    assert_eq!(vp9_level_for(7680, 4320, 60.0), 60);

    // Profile 0・8bit・BT.709・limited のキーフレーム非圧縮ヘッダ
    // frame_marker=10, profile_low=0, profile_high=0, show_existing=0, frame_type=0, show=1, err_res=0 => 0x82
    // sync=0x49,0x83,0x42, color_space=2 (010), color_range=0 => 0b0100_0000 = 0x40
    let kf_p0 = [0x82, 0x49, 0x83, 0x42, 0x40];
    assert_eq!(
        parse_vp9_frame_header(&kf_p0),
        Some(Vp9FrameHeader {
            profile: 0,
            bit_depth: 8,
            chroma_subsampling: 0,
            full_range: false,
            primaries: 1,
            transfer: 1,
            matrix: 1,
        })
    );

    // Profile 2・10bit・BT.2020・limited のキーフレーム
    // frame_marker=10, profile_low=0, profile_high=1, show_existing=0, frame_type=0, show=1, err_res=0 => 0x92
    // sync=0x49,0x83,0x42, ten_or_twelve=0, color_space=5 (101), color_range=0 => 0b0101_0000 = 0x50
    let kf_p2 = [0x92, 0x49, 0x83, 0x42, 0x50];
    let hdr_p2 = parse_vp9_frame_header(&kf_p2).unwrap();
    assert_eq!(hdr_p2.profile, 2);
    assert_eq!(hdr_p2.bit_depth, 10);
    assert_eq!(
        (hdr_p2.primaries, hdr_p2.transfer, hdr_p2.matrix),
        (9, 14, 9)
    );

    // Matroska Colour の HDR PQ（transfer=16）と 4:2:0 colocated（1）は上書きせず保護する
    let mut vpcc = [0, 50, (8 << 4) | (1 << 1), 9, 16, 9, 0, 0];
    merge_vp9_header_into_vpcc(&mut vpcc, hdr_p2);
    assert_eq!(vpcc, [2, 50, (10 << 4) | (1 << 1), 9, 16, 9, 0, 0]);
    assert_eq!(
        vpcc_color_info(&vpcc),
        Some(ColorInfo {
            primaries: 9,
            transfer: 16,
            matrix: 9,
            full_range: false,
        })
    );
    assert_eq!(vpcc_color_info(&[0, 10, 0x80, 2, 2, 2, 0, 0]), None);

    // スーパーフレーム（1 枚目が非キー、2 枚目がキーフレーム）からキーフレームを探せる
    let non_kf = [0x86, 0x00]; // frame_type = 1
    let mut superframe = Vec::new();
    superframe.extend_from_slice(&non_kf);
    superframe.extend_from_slice(&kf_p0);
    // marker: 0b110_00_001 = 0xC1（1 バイト長 × 2 フレーム）
    superframe.extend_from_slice(&[0xC1, non_kf.len() as u8, kf_p0.len() as u8, 0xC1]);
    assert_eq!(
        parse_vp9_frame_header(&superframe),
        Some(Vp9FrameHeader {
            profile: 0,
            bit_depth: 8,
            chroma_subsampling: 0,
            full_range: false,
            primaries: 1,
            transfer: 1,
            matrix: 1,
        })
    );
    // フレーム長の合計がペイロード長と一致しない偽のインデックスは弾く
    let mut bad_superframe = vec![0x00];
    bad_superframe.extend_from_slice(&superframe);
    assert_eq!(parse_vp9_frame_header(&bad_superframe), None);

    // AlphaMode = 1 の透過 VP9 MKV は WebKit が透過デコード非対応のためエラーにする
    let ebml_alpha_vp9: &[u8] = &[
        0x1A, 0x45, 0xDF, 0xA3, 0x93, // EBML (19 bytes)
        0x42, 0x82, 0x88, b'm', b'a', b't', b'r', b'o', b's', b'k', b'a', // DocType
        0x42, 0x87, 0x81, 0x04, // DocTypeVersion = 4
        0x42, 0x85, 0x81, 0x02, // DocTypeReadVersion = 2
        0x18, 0x53, 0x80, 0x67, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, // Segment
        0x15, 0x49, 0xA9, 0x66, 0x88, // Info (8 bytes)
        0x4D, 0x80, 0x81, b't', // MuxingApp = "t"
        0x57, 0x41, 0x81, b't', // WritingApp = "t"
        0x16, 0x54, 0xAE, 0x6B, 0x9F, // Tracks (31 bytes)
        0xAE, 0x9D, // TrackEntry (29 bytes)
        0xD7, 0x81, 0x01, // TrackNumber = 1
        0x73, 0xC5, 0x81, 0x01, // TrackUID = 1
        0x83, 0x81, 0x01, // TrackType = Video (1)
        0x86, 0x85, b'V', b'_', b'V', b'P', b'9', // CodecID = "V_VP9"
        0xE0, 0x8A, // Video (10 bytes)
        0xB0, 0x81, 0x40, // PixelWidth = 64
        0xBA, 0x81, 0x40, // PixelHeight = 64
        0x53, 0xC0, 0x81, 0x01, // AlphaMode = 1
        0x1F, 0x43, 0xB6, 0x75, 0x84, 0xE7, 0x81, 0x00, // Cluster + Timestamp = 0
    ];
    let mkv_alpha =
        matroska_demuxer::MatroskaFile::open(std::io::Cursor::new(ebml_alpha_vp9)).unwrap();
    let err = remux_video_track(&mkv_alpha.tracks()[0]).unwrap_err();
    assert!(err.contains("VP9"), "err={err}");
}

#[test]
fn vp9_mkv_is_remuxed_to_fragmented_mp4() {
    let path = remux_testdata("vp9.mkv");
    let (mut stream, opened) = MkvStream::open(&path, None).unwrap();
    assert_eq!(opened.video_codec, "vp09.00.10.08");
    assert_eq!(opened.audio_codec.as_deref(), Some("opus"));
    assert!(!opened.transcoded);
    assert!(!opened.can_transcode);

    // 初期化セグメントに vp09 と FullBox の vpcC（version=1, flags=0, BT.709）および colr（nclx）が入る
    let init = stream.init_segment();
    assert!(init.windows(4).any(|w| w == b"vp09"));
    assert!(init.windows(4).any(|w| w == b"colr"));
    let vpcc_pos = init
        .windows(4)
        .position(|w| w == b"vpcC")
        .expect("init セグメントに vpcC が含まれること");
    let vpcc_size = u32::from_be_bytes(init[vpcc_pos - 4..vpcc_pos].try_into().unwrap()) as usize;
    assert_eq!(vpcc_size, 20); // header(8) + version/flags(4) + payload(8)
    assert_eq!(&init[vpcc_pos + 4..vpcc_pos + 8], &[1, 0, 0, 0]);
    let vpcc_body = &init[vpcc_pos + 8..vpcc_pos + 16];
    assert_eq!(vpcc_body[0], 0); // profile 0
    assert_eq!(vpcc_body[1], 10); // level 1.0 (64x64 24fps)
    assert_eq!(vpcc_body[2] >> 4, 8); // 8-bit
    assert_eq!(&vpcc_body[3..6], &[1, 1, 1]); // BT.709

    // 全断片を順に読み切って全 72 映像フレーム + 音声が連続して得られる
    let mut video_count = 0usize;
    let mut video_end: Option<i64> = None;
    let mut audio_end: Option<i64> = None;
    let mut fragments = 0usize;
    loop {
        let fragment = stream.read_fragment().unwrap();
        if fragment.is_empty() {
            break;
        }
        fragments += 1;
        let tracks = parse_fragment(&fragment);
        let v = &tracks[&1];
        assert!(v[0].keyframe);
        assert!(v.iter().all(|s| s.pts == s.dts));
        if let Some(end) = video_end {
            assert_eq!(v[0].dts, end);
        }
        let v_last = v.last().unwrap();
        video_end = Some(v_last.dts + v_last.duration);
        video_count += v.len();
        if let Some(a) = tracks.get(&2) {
            if let Some(end) = audio_end {
                assert_eq!(a[0].dts, end);
            }
            let a_last = a.last().unwrap();
            audio_end = Some(a_last.dts + a_last.duration);
        }
    }
    assert!(fragments >= 2);
    assert_eq!(video_count, 72);
    assert!(audio_end.unwrap() > 96_000);

    // 途中（1.25 秒）へのシークは直前のキーフレーム（1.0 秒）から始まる
    let sought = stream.seek(1.25).unwrap();
    assert!((sought - 1.0).abs() < 0.05, "sought={sought}");
    let after_seek = stream.read_fragment().unwrap();
    let tracks = parse_fragment(&after_seek);
    assert!(tracks[&1][0].keyframe);
    let first_sec = tracks[&1][0].pts as f64 / 90_000.0;
    assert!((first_sec - 1.0).abs() < 0.05, "first_sec={first_sec}");
}

// ファイルの情報の値を読みやすい文字列にする

/// 3 桁ごとにカンマを入れる
pub(crate) fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// 小数を最大 `digits` 桁で書き、末尾の 0 と小数点を落とす（23.976 / 30 / 44.1）
pub(crate) fn trim_float(v: f64, digits: usize) -> String {
    let s = format!("{v:.digits$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// ファイルの大きさ（「12.3 MB（12,345,678 バイト）」）。単位は 1024 倍ごと
pub(crate) fn format_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if n < 1024 {
        return format!("{} バイト", group_digits(n));
    }
    let mut v = n as f64 / 1024.0;
    let mut unit = UNITS[0];
    for u in &UNITS[1..] {
        if v < 1024.0 {
            break;
        }
        v /= 1024.0;
        unit = u;
    }
    let digits = if v < 10.0 { 2 } else { 1 };
    format!("{v:.digits$} {unit}（{} バイト）", group_digits(n))
}

/// 長さ（「1:02:03」「3:25」）。1 秒未満は切り捨てる
pub(crate) fn format_duration(seconds: f64) -> Option<String> {
    if !seconds.is_finite() || seconds <= 0.0 {
        return None;
    }
    let total = seconds.floor() as u64;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    Some(if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    })
}

/// ビットレート（bps → 「320 kbps」「12.3 Mbps」）
pub(crate) fn format_bitrate(bps: f64) -> Option<String> {
    if !bps.is_finite() || bps <= 0.0 {
        return None;
    }
    Some(if bps >= 10_000_000.0 {
        format!("{} Mbps", trim_float(bps / 1_000_000.0, 1))
    } else {
        format!("{} kbps", group_digits((bps / 1000.0).round() as u64))
    })
}

/// サンプリング周波数（「48 kHz」「44.1 kHz」）
pub(crate) fn format_sample_rate(hz: f64) -> Option<String> {
    if !hz.is_finite() || hz <= 0.0 {
        return None;
    }
    Some(format!("{} kHz", trim_float(hz / 1000.0, 3)))
}

/// チャンネル数（「2（ステレオ）」「6（5.1ch）」）
pub(crate) fn format_channels(channels: u64) -> Option<String> {
    let name = match channels {
        0 => return None,
        1 => "モノラル",
        2 => "ステレオ",
        6 => "5.1ch",
        8 => "7.1ch",
        _ => return Some(channels.to_string()),
    };
    Some(format!("{channels}（{name}）"))
}

/// フレームレート（「23.976 fps」）
pub(crate) fn format_fps(fps: f64) -> Option<String> {
    if !fps.is_finite() || fps <= 0.0 || fps > 10_000.0 {
        return None;
    }
    Some(format!("{} fps", trim_float(fps, 3)))
}

/// 解像度（「1920 × 1080」）
pub(crate) fn format_size(width: u64, height: u64) -> Option<String> {
    (width > 0 && height > 0).then(|| format!("{width} × {height}"))
}

/// 縦横比（「16:9」）。きれいな比にならなければ小数で（「2.39:1」）
pub(crate) fn format_aspect(width: u64, height: u64) -> Option<String> {
    if width == 0 || height == 0 {
        return None;
    }
    fn gcd(a: u64, b: u64) -> u64 {
        if b == 0 { a } else { gcd(b, a % b) }
    }
    let g = gcd(width, height);
    let (w, h) = (width / g, height / g);
    if w <= 32 && h <= 32 {
        return Some(format!("{w}:{h}"));
    }
    Some(format!("{}:1", trim_float(width as f64 / height as f64, 2)))
}

/// UNIX 時刻（ミリ秒）を UTC の「YYYY-MM-DD HH:MM:SS UTC」にする。
/// フロントエンドは time から利用者の地域の時刻で出し直すので、これは控え
pub(crate) fn format_utc(unix_ms: f64) -> String {
    let secs = (unix_ms / 1000.0).floor() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// 1970-01-01 からの日数を（年, 月, 日）にする（Howard Hinnant の civil_from_days）
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

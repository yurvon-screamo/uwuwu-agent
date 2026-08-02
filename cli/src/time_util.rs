pub fn format_date_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = now / 86400;

    let (year, month, day) = days_to_ymd(days as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

pub fn format_timestamp_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mins = (now / 60) % 60;
    let hours = (now / 3600) % 24;
    let days = now / 86400;

    let (year, month, day) = days_to_ymd(days as i64);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{mins:02}")
}

pub fn days_to_ymd(days: i64) -> (i64, u32, u32) {
    let mut y = 1970i64;
    let mut d = days;

    loop {
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let yd = if leap { 366 } else { 365 };
        if d < yd {
            break;
        }
        d -= yd;
        y += 1;
    }

    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let month_days = [31, 28 + leap as i64, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

    let mut m = 0;
    while m < 12 && d >= month_days[m] {
        d -= month_days[m];
        m += 1;
    }

    (y, (m + 1) as u32, (d + 1) as u32)
}

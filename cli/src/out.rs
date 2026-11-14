//! stdout-печать cli с гвардом broken pipe (задача 1093).
//!
//! rust ставит SIGPIPE в SIG_IGN на старте процесса, поэтому write в пайп,
//! закрытый потребителем (head/less), не убивает процесс сигналом, а
//! возвращает EPIPE — std-макросы println!/print! на нём паникуют
//! (rc 101 + стектрейс в stderr), и rc-гарды агентов трактуют это как
//! провал команды. Канонический паттерн rust-доков «Handling broken
//! pipes»: ошибка записи перехватывается как значение, BrokenPipe — тихий
//! успех, потребитель получил всё, что успел прочитать.
//!
//! Контракт зеркален std: на здоровом пайпе/TTY поведение байт-в-байт
//! прежнее; прочие ошибки записи (ENOSPC, EBADF…) — прежняя громкая паника.

use std::fmt::Arguments;
use std::io::{self, Write};

/// Печать строки в stdout с гвардом broken pipe (зеркало println!).
macro_rules! outln {
    () => {
        $crate::out::line(format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::out::line(format_args!($($arg)*))
    };
}

pub(crate) use outln;

/// Печать в stdout без переноса строки, с гвардом broken pipe (зеркало
/// print!).
macro_rules! out {
    ($($arg:tt)*) => {
        $crate::out::raw(format_args!($($arg)*))
    };
}

pub(crate) use out;

pub(crate) fn line(args: Arguments<'_>) {
    if let Err(err) = write_args(args, true) {
        on_write_failure(err);
    }
}

pub(crate) fn raw(args: Arguments<'_>) {
    if let Err(err) = write_args(args, false) {
        on_write_failure(err);
    }
}

/// Тот же write-путь, что у std-макросов: lock глобального stdout,
/// format-args; LineWriter сам флашит по '\n', для `raw` без '\n' буфер
/// не флашится (паритет с print!).
fn write_args(args: Arguments<'_>, newline: bool) -> io::Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    out.write_fmt(args)?;
    if newline {
        out.write_all(b"\n")?;
    }
    Ok(())
}

/// BrokenPipe — потребитель закрыл конец пайпа: тихий успех rc 0.
/// Прочие ошибки записи — громкая паника, паритет с println!/print!.
fn on_write_failure(err: io::Error) -> ! {
    if err.kind() == io::ErrorKind::BrokenPipe {
        std::process::exit(0);
    }
    panic!("failed printing to stdout: {err}");
}

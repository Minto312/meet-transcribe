//! Starting with Windows, so the model is compiled before the meeting is.
//!
//! The NPU takes ~170 s to compile `medium` and nothing caches that between
//! processes. Launching at sign-in and preloading straight away moves the wait
//! to a moment nobody is waiting on.
//!
//! The entry is an ordinary `HKCU\…\Run` value — what Task Manager's startup
//! page lists and toggles — so the user can see and undo it there too. That is
//! also why the registry, not `settings.json`, is the record of whether it is on.

use std::io::ErrorKind;

use anyhow::{Context, Result};
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};
use winreg::{RegKey, RegValue};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Where Task Manager records that it switched a `Run` entry off, without
/// touching the entry itself.
const APPROVED_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const VALUE: &str = "meet-transcribe";
/// What the `Run` entry passes, so the window starts minimized instead of
/// landing on top of whatever the user signed in to do.
pub const FLAG: &str = "--autostart";

fn command_line() -> Result<String> {
    let exe = std::env::current_exe().context("実行ファイルのパスを取得できません")?;
    Ok(format!("\"{}\" {FLAG}", exe.display()))
}

/// True when Windows will start *this* executable at sign-in: the entry exists,
/// points here rather than at another copy (a checkout, an older install), and
/// has not been switched off in Task Manager.
pub fn is_enabled() -> bool {
    let Ok(expected) = command_line() else {
        return false;
    };
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let registered = hkcu
        .open_subkey_with_flags(RUN_KEY, KEY_READ)
        .and_then(|key| key.get_value::<String, _>(VALUE))
        .is_ok_and(|value| value.eq_ignore_ascii_case(&expected));
    // The first byte is 0x02 while enabled and 0x03 once disabled.
    let disabled = hkcu
        .open_subkey_with_flags(APPROVED_KEY, KEY_READ)
        .and_then(|key| key.get_raw_value(VALUE))
        .is_ok_and(|value| value.bytes.first() == Some(&0x03));
    registered && !disabled
}

pub fn set_enabled(enabled: bool) -> Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (run, _) = hkcu
        .create_subkey(RUN_KEY)
        .context("自動起動のレジストリキーを開けません")?;
    if enabled {
        run.set_value(VALUE, &command_line()?)
            .context("自動起動を登録できません")?;
    } else {
        delete(&run, "自動起動を解除できません")?;
    }
    // Clearing Task Manager's mark either way: when turning it on, an old
    // "disabled" would silently win; when turning it off, it is litter.
    if let Ok(approved) = hkcu.open_subkey_with_flags(APPROVED_KEY, KEY_SET_VALUE) {
        delete(&approved, "タスク マネージャーの無効化の記録を消せません")?;
    }
    Ok(())
}

/// Both registry values exactly as found, so `--startup-check` can put back an
/// entry it does not own — say, one pointing at the installed copy while the
/// check runs from a checkout.
pub struct Saved {
    run: Option<RegValue>,
    approved: Option<RegValue>,
}

pub fn save() -> Saved {
    let read = |path: &str| {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(path, KEY_READ)
            .and_then(|key| key.get_raw_value(VALUE))
            .ok()
    };
    Saved {
        run: read(RUN_KEY),
        approved: read(APPROVED_KEY),
    }
}

pub fn restore(saved: &Saved) -> Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    for (path, value) in [(RUN_KEY, &saved.run), (APPROVED_KEY, &saved.approved)] {
        let (key, _) = hkcu
            .create_subkey(path)
            .with_context(|| format!("{path} を開けません"))?;
        match value {
            Some(value) => key
                .set_raw_value(VALUE, value)
                .with_context(|| format!("{path} を元に戻せません"))?,
            None => delete(&key, "自動起動の登録を元に戻せません")?,
        }
    }
    Ok(())
}

fn delete(key: &RegKey, context: &'static str) -> Result<()> {
    match key.delete_value(VALUE) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).context(context),
    }
}

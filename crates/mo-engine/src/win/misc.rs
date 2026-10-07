//! 杂项：磁盘空间、提权检测/重启、单实例互斥体、自删。

use std::path::Path;
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::PCWSTR;

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 目录所在磁盘的剩余可用字节数（用户配额感知）。
pub fn disk_free_bytes(dir: &Path) -> Result<u64, String> {
    let w = to_wide(&dir.to_string_lossy());
    let mut free: u64 = 0;
    let r =
        unsafe { GetDiskFreeSpaceExW(PCWSTR(w.as_ptr()), Some(&mut free as *mut u64), None, None) };
    r.map_err(|e| format!("GetDiskFreeSpaceExW 失败: {e}"))
        .map(|_| free)
}

/// 当前进程是否以管理员令牌运行。
pub fn is_elevated() -> bool {
    use windows::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elev = TOKEN_ELEVATION::default();
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elev as *mut _ as *mut core::ffi::c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut 0u32,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elev.TokenIsElevated != 0
    }
}

/// Windows 命令行参数引用：还原 CommandLineToArgvW 的切分边界。
/// 含空格/制表/引号的参数必须加引号并对反斜杠-引号序列做转义，
/// 否则 `/DIR=D:\My Apps\Pymss` 在提权重启后会被拆成 `D:\My`。
fn quote_arg(s: &str) -> String {
    if s.is_empty() {
        return "\"\"".into();
    }
    if !s.contains([' ', '\t', '"']) {
        return s.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0usize;
    for c in s.chars() {
        match c {
            '\\' => {
                backslashes += 1;
                out.push('\\');
            }
            '"' => {
                for _ in 0..=backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push('"');
            }
            _ => {
                backslashes = 0;
                out.push(c);
            }
        }
    }
    for _ in 0..=backslashes {
        out.push('\\');
    }
    out.push('"');
    out
}

fn quoted_params(args: &[String]) -> String {
    args.iter()
        .map(|a| quote_arg(a))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 以管理员重启自身（UAC 弹窗，不等待）。成功后调用方应立即退出。
pub fn relaunch_elevated(exe: &Path, args: &[String]) -> Result<(), String> {
    let verb = to_wide("runas");
    let file = to_wide(&exe.to_string_lossy());
    let params_w = to_wide(&quoted_params(args));
    let r = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR(params_w.as_ptr()),
            None,
            SW_SHOWNORMAL,
        )
    };
    if r.0 as isize > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecuteW runas 失败: {:?}", r.0))
    }
}

/// 静默模式的提权重启：等待提权子进程结束并返回其退出码。
/// ShellExecuteW 不等待子进程，部署脚本会在安装真正完成前拿到假成功。
pub fn relaunch_elevated_wait(exe: &Path, args: &[String]) -> Result<Option<i32>, String> {
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};

    let verb = to_wide("runas");
    let file = to_wide(&exe.to_string_lossy());
    let params_w = to_wide(&quoted_params(args));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params_w.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe {
        if ShellExecuteExW(&mut info).is_ok() {
            if info.hProcess.is_invalid() {
                return Ok(None); // 已提权重启等价场景，无句柄可等
            }
            let _ = WaitForSingleObject(info.hProcess, u32::MAX);
            let mut code: u32 = 0;
            if GetExitCodeProcess(info.hProcess, &mut code).is_ok() {
                let _ = CloseHandle(info.hProcess);
                return Ok(Some(code as i32));
            }
            let _ = CloseHandle(info.hProcess);
            Ok(None)
        } else {
            Err("ShellExecuteExW runas 失败".into())
        }
    }
}

/// 单实例互斥体 guard。已存在同名互斥体时返回 None。
pub struct SingleInstance {
    handle: HANDLE,
}

impl SingleInstance {
    pub fn new(name: &str) -> Option<Self> {
        let full = format!("Global\\{name}");
        let w = to_wide(&full);
        unsafe {
            // CreateMutexW 在同名互斥体已存在时同样返回有效句柄，
            // 「已存在」只通过 GetLastError 报告——成功分支必须立即检查。
            match CreateMutexW(None, false, PCWSTR(w.as_ptr())) {
                Ok(h) => {
                    if windows::Win32::Foundation::GetLastError() == ERROR_ALREADY_EXISTS {
                        let _ = CloseHandle(h);
                        return None;
                    }
                    Some(SingleInstance { handle: h })
                }
                Err(_) => None,
            }
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.handle);
            let _ = CloseHandle(self.handle);
        }
    }
}

/// 原生弹窗。kind: "confirm" 返回用户是否点了确定；其余仅展示并返回 true。
pub fn message_box(text: &str, kind: &str) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        IDOK, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_OKCANCEL, MessageBoxW,
    };
    let text_w = to_wide(text);
    let caption = to_wide("MoInstaller");
    let (flags, confirm) = match kind {
        "error" => (MB_ICONERROR, false),
        "warn" | "warning" => (MB_ICONWARNING, false),
        "confirm" => (MB_ICONINFORMATION | MB_OKCANCEL, true),
        _ => (MB_ICONINFORMATION | MB_OK, false),
    };
    unsafe {
        let r = MessageBoxW(
            None,
            PCWSTR(text_w.as_ptr()),
            PCWSTR(caption.as_ptr()),
            flags,
        );
        if confirm { r == IDOK } else { true }
    }
}

/// 自删：延迟约一秒后由 cmd 删除自身（卸载收尾）。
///
/// 用 ping 而非 timeout 做延迟——timeout 在 stdin 被重定向时会立即报错退出；
/// del 失败（exe 尚未完全退出/杀软短暂锁定）再重试一次。
pub fn self_delete(exe: &Path) {
    let p = exe.to_string_lossy();
    let parent = exe
        .parent()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    let cmd = format!(
        "/c ping -n 2 127.0.0.1 >nul & (del /f /q \"{p}\" || (ping -n 2 127.0.0.1 >nul & del /f /q \"{p}\")) & rmdir \"{parent}\""
    );
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = std::process::Command::new("cmd")
        .raw_arg(cmd)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_free_of_temp() {
        let free = disk_free_bytes(&std::env::temp_dir()).unwrap();
        assert!(free > 1024 * 1024, "temp 盘剩余 {free} 字节，应大于 1MB");
    }

    #[test]
    fn self_delete_works() {
        let f = std::env::temp_dir().join(format!("mo-sd-test-{}.dat", std::process::id()));
        std::fs::write(&f, b"x").unwrap();
        self_delete(&f);
        for _ in 0..20 {
            if !f.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        assert!(!f.exists(), "自删失败: {}", f.display());
    }

    #[test]
    fn single_instance_mutex_smoke() {
        let name = format!("MoInstaller.Test.{}", std::process::id());
        let a = SingleInstance::new(&name);
        assert!(a.is_some());
        drop(a);
    }
}

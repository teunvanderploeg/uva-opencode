//! Windows launch shim. Only the helper inherits the two CDP handles; its private
//! job object kills Chromium and its children if the controlling process exits.
use anyhow::{Result, anyhow};
use std::{
    ffi::{OsStr, OsString},
    mem::{size_of, zeroed},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{
        GENERIC_READ, GENERIC_WRITE, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
        SetHandleInformation, WAIT_OBJECT_0,
    },
    Security::SECURITY_ATTRIBUTES,
    Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_PIPE,
        GetFileType, OPEN_EXISTING,
    },
    System::{
        Console::{GetStdHandle, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE},
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        },
        Threading::{
            CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, GetExitCodeProcess,
            INFINITE, PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOW,
            TerminateProcess, WaitForSingleObject,
        },
    },
};

fn wide(value: &OsStr) -> Result<Vec<u16>> {
    let mut result: Vec<_> = value.encode_wide().collect();
    anyhow::ensure!(!result.contains(&0), "Invalid browser process argument.");
    result.push(0);
    Ok(result)
}

/// Standard Windows CRT quoting, including quotes and trailing backslashes.
fn command_line(args: &[OsString]) -> Result<Vec<u16>> {
    let mut result = Vec::new();
    for (index, arg) in args.iter().enumerate() {
        if index > 0 {
            result.push(b' ' as u16);
        }
        result.push(b'"' as u16);
        let mut backslashes = 0;
        for unit in arg.encode_wide() {
            anyhow::ensure!(unit != 0, "Invalid browser process argument.");
            if unit == b'\\' as u16 {
                backslashes += 1;
                continue;
            }
            if unit == b'"' as u16 {
                result.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2 + 1));
            } else {
                result.extend(std::iter::repeat_n(b'\\' as u16, backslashes));
            }
            backslashes = 0;
            result.push(unit);
        }
        result.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2));
        result.push(b'"' as u16);
    }
    result.push(0);
    Ok(result)
}

fn owned(handle: HANDLE) -> Result<OwnedHandle> {
    anyhow::ensure!(
        !handle.is_null() && handle != INVALID_HANDLE_VALUE,
        "Could not create the private browser process resources."
    );
    // SAFETY: Callers pass a newly-created handle, with ownership transferred once.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

pub fn run(args: &[OsString]) -> Result<i32> {
    anyhow::ensure!(!args.is_empty(), "Missing browser executable.");
    // SAFETY: These are borrowed standard handles, installed by the parent for this shim.
    let (read, write) = unsafe {
        (
            GetStdHandle(STD_INPUT_HANDLE),
            GetStdHandle(STD_OUTPUT_HANDLE),
        )
    };
    anyhow::ensure!(
        unsafe { GetFileType(read) } == FILE_TYPE_PIPE
            && unsafe { GetFileType(write) } == FILE_TYPE_PIPE,
        "Private browser pipes were not supplied."
    );
    anyhow::ensure!(
        unsafe { SetHandleInformation(read, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } != 0
            && unsafe { SetHandleInformation(write, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) }
                != 0,
        "Could not inherit the private browser pipes."
    );
    let job = owned(unsafe { CreateJobObjectW(null(), null()) })?;
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    anyhow::ensure!(
        unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } != 0,
        "Could not configure browser cleanup."
    );
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    let nul = wide(OsStr::new("NUL"))?;
    let null_io = owned(unsafe {
        CreateFileW(
            nul.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &attributes,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    })?;
    let mut startup: STARTUPINFOW = unsafe { zeroed() };
    startup.cb = size_of::<STARTUPINFOW>() as u32;
    startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdInput = null_io.as_raw_handle();
    startup.hStdOutput = null_io.as_raw_handle();
    startup.hStdError = null_io.as_raw_handle();
    let application = wide(&args[0])?;
    let mut arguments = args.to_vec();
    arguments.push(
        format!(
            "--remote-debugging-io-pipes={},{}",
            read as usize, write as usize
        )
        .into(),
    );
    let mut line = command_line(&arguments)?;
    let mut process: PROCESS_INFORMATION = unsafe { zeroed() };
    // Only the shim's standard pipe handles and NUL are inheritable. The job handle is private.
    anyhow::ensure!(
        unsafe {
            CreateProcessW(
                application.as_ptr(),
                line.as_mut_ptr(),
                null(),
                null(),
                1,
                CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
                null(),
                null(),
                &startup,
                &mut process,
            )
        } != 0,
        "Could not launch Chromium."
    );
    let child = owned(process.hProcess)?;
    let thread = owned(process.hThread)?;
    if unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) } == 0 {
        unsafe { TerminateProcess(child.as_raw_handle(), 1) };
        return Err(anyhow!("Could not attach browser process cleanup."));
    }
    anyhow::ensure!(
        unsafe { ResumeThread(thread.as_raw_handle()) } != u32::MAX,
        "Could not start Chromium."
    );
    anyhow::ensure!(
        unsafe { WaitForSingleObject(child.as_raw_handle(), INFINITE) } == WAIT_OBJECT_0,
        "Could not wait for Chromium."
    );
    let mut code = 1;
    anyhow::ensure!(
        unsafe { GetExitCodeProcess(child.as_raw_handle(), &mut code) } != 0,
        "Could not read the browser exit status."
    );
    Ok(code as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quotes_and_trailing_backslashes_survive_windows_argument_encoding() {
        let line = command_line(&[
            r#"C:\Program Files\chrome.exe"#.into(),
            r#"a"b"#.into(),
            r#"C:\with space\"#.into(),
        ])
        .unwrap();
        let text = String::from_utf16(&line[..line.len() - 1]).unwrap();
        assert_eq!(
            text,
            r#""C:\Program Files\chrome.exe" "a\"b" "C:\with space\\""#
        );
    }
}

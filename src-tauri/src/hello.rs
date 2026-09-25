#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelloAvailability {
    #[allow(dead_code)]
    Available,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelloPrompt {
    Verified,
    Canceled,
    Unavailable,
}

pub(crate) fn prompt_from_hresult(code: i32) -> HelloPrompt {
    if code == 0 {
        HelloPrompt::Verified
    } else if code == -2147023673 {
        HelloPrompt::Canceled
    } else {
        HelloPrompt::Unavailable
    }
}

#[cfg(not(windows))]
pub async fn availability() -> HelloAvailability {
    HelloAvailability::Unavailable
}

#[cfg(not(windows))]
pub async fn request_verification() -> HelloPrompt {
    HelloPrompt::Unavailable
}

#[cfg(windows)]
pub async fn availability() -> HelloAvailability {
    use windows::Security::Credentials::UI::{UserConsentVerifier, UserConsentVerifierAvailability};

    let operation = match UserConsentVerifier::CheckAvailabilityAsync() {
        Ok(operation) => operation,
        Err(_) => return HelloAvailability::Unavailable,
    };

    match operation.await {
        Ok(UserConsentVerifierAvailability::Available) => HelloAvailability::Available,
        _ => HelloAvailability::Unavailable,
    }
}

#[cfg(windows)]
pub async fn request_verification(app: &tauri::AppHandle) -> HelloPrompt {
    use tauri::Manager;
    use windows::core::HSTRING;
    use windows::Security::Credentials::UI::{UserConsentVerificationResult, UserConsentVerifier};
    use windows::Win32::System::WinRT::IUserConsentVerifierInterop;
    use windows_future::IAsyncOperation;

    let hwnd = {
        let Some(window) = app.get_webview_window("main") else {
            return HelloPrompt::Unavailable;
        };
        match window.hwnd() {
            Ok(hwnd) if !hwnd.is_invalid() => hwnd,
            _ => return HelloPrompt::Unavailable,
        }
    };

    let interop = match windows::core::factory::<UserConsentVerifier, IUserConsentVerifierInterop>() {
        Ok(interop) => interop,
        Err(error) => return prompt_from_hresult(error.code().0),
    };

    // SAFETY: `hwnd` is the process main window, and this is the documented Win32 interop for UserConsentVerifier.
    let operation: IAsyncOperation<UserConsentVerificationResult> =
        match unsafe { interop.RequestVerificationForWindowAsync(hwnd, &HSTRING::from("Unlock DBX")) } {
            Ok(operation) => operation,
            Err(error) => return prompt_from_hresult(error.code().0),
        };

    match operation.await {
        Ok(UserConsentVerificationResult::Verified) => HelloPrompt::Verified,
        Ok(UserConsentVerificationResult::Canceled) => HelloPrompt::Canceled,
        Ok(_) => HelloPrompt::Unavailable,
        Err(error) => prompt_from_hresult(error.code().0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn non_windows_availability_is_unavailable() {
        if cfg!(windows) {
            return;
        }
        assert_eq!(availability().await, HelloAvailability::Unavailable);
    }

    #[tokio::test]
    async fn non_windows_request_verification_is_unavailable() {
        if cfg!(windows) {
            return;
        }
        assert_eq!(request_verification().await, HelloPrompt::Unavailable);
    }

    #[test]
    fn success_hresult_maps_to_verified() {
        assert_eq!(prompt_from_hresult(0), HelloPrompt::Verified);
    }

    #[test]
    fn canceled_hresult_maps_to_canceled() {
        assert_eq!(prompt_from_hresult(-2147023673), HelloPrompt::Canceled); // HRESULT_FROM_WIN32(ERROR_CANCELLED)
    }

    #[test]
    fn other_hresult_maps_to_unavailable() {
        assert_eq!(prompt_from_hresult(0x80004005u32 as i32), HelloPrompt::Unavailable);
        // E_FAIL
    }
}

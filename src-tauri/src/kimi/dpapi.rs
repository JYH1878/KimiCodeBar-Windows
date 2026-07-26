//! DPAPI（Windows Data Protection API）封装：credentials.json 的本地加密存储。
//!
//! DPAPI 以【当前 Windows 用户】的凭据派生密钥加密（CurrentUser 作用域，默认，
//! 不传 CRYPTPROTECT_LOCAL_MACHINE）：密文只能由同一台机器上的同一用户解开；
//! 更换 Windows 用户、重装系统、或把文件拷贝到其他机器后均无法解密。
//! 其生命周期与 Windows 凭据管理器（API Key 的存储位置）一致——
//! 密文失效后用户重新登录即可重建凭证。

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB,
    };

    /// 禁止 DPAPI 弹出任何交互 UI（托盘为后台进程，失败应直接返回错误）
    const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;

    /// CryptProtectData 加密（CurrentUser 作用域）
    pub fn protect(plaintext: &[u8]) -> Result<Vec<u8>, String> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: plaintext.len() as u32,
            // API 声明为 *mut，但 CryptProtectData 对输入只读
            pbData: plaintext.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        // SAFETY: input 指向 plaintext 的有效内存；成功后 output.pbData 由系统分配
        // （长度 output.cbData），拷贝后立即用 LocalFree 释放
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),     // 无描述串
                std::ptr::null(),     // 无额外熵
                std::ptr::null_mut(), // 保留参数
                std::ptr::null(),     // 无提示结构
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 {
            return Err(format!(
                "CryptProtectData 失败: {}",
                std::io::Error::last_os_error()
            ));
        }
        let ciphertext =
            unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
        unsafe { LocalFree(output.pbData as *mut std::ffi::c_void) };
        Ok(ciphertext)
    }

    /// CryptUnprotectData 解密（只能解开同一 Windows 用户加密的密文）
    pub fn unprotect(ciphertext: &[u8]) -> Result<Vec<u8>, String> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: ciphertext.len() as u32,
            pbData: ciphertext.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        // SAFETY: 同 protect
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(), // 不需要返回描述串
                std::ptr::null(),     // 无额外熵
                std::ptr::null_mut(), // 保留参数
                std::ptr::null(),     // 无提示结构
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 {
            return Err(format!(
                "CryptUnprotectData 失败: {}",
                std::io::Error::last_os_error()
            ));
        }
        let plaintext =
            unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
        unsafe { LocalFree(output.pbData as *mut std::ffi::c_void) };
        Ok(plaintext)
    }
}

// 本应用仅面向 Windows；非 Windows 下 protect 原样透传、unprotect 恒失败，
// 使 load 走明文解析回退路径（等价于旧版本行为），仅保证可编译、测试可跑。
#[cfg(not(windows))]
mod imp {
    pub fn protect(plaintext: &[u8]) -> Result<Vec<u8>, String> {
        Ok(plaintext.to_vec())
    }

    pub fn unprotect(_ciphertext: &[u8]) -> Result<Vec<u8>, String> {
        Err("DPAPI 仅在 Windows 上可用".to_string())
    }
}

pub use imp::{protect, unprotect};

# 关键问题修复清单

这个文档列出了需要立即修复的关键代码质量问题。

## 🔴 严重 Bug（必须立即修复）

### 1. State::Display 无限递归

**位置**: `src/state.rs` 第 11-15 行

**当前代码**:
```rust
impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.clone())  // ❌ 无限递归！
    }
}
```

**问题**: 调用 `write!(f, "{}", self.clone())` 会再次调用 `fmt::Display`，导致栈溢出。

**修复**:
```rust
impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            State::Init => write!(f, "Init"),
            State::Login => write!(f, "Login"),
        }
    }
}
```

或者使用 strum crate:
```rust
use strum_macros::Display;

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, PartialOrd, Display)]
pub enum State {
    Init,
    Login,
}
```

---

### 2. main.rs 中的日志消息错误

**位置**: `src/main.rs` 第 169 行

**当前代码**:
```rust
log::info!("ctrl+v received");  // ❌ 应该是 Ctrl+C
```

**修复**:
```rust
log::info!("ctrl+c received");
```

---

## 🔴 内存安全问题

### 3. FFI 内存泄漏

**位置**: `src/wg.rs` 第 29-40 行

**当前代码**:
```rust
unsafe fn to_c_char_array(data: &[u8]) -> *const c_char {
    CString::from_vec_unchecked(data.to_vec()).into_raw() as *const c_char
}

fn uapi(buff: &[u8]) -> Vec<u8> {
    unsafe {
        let s = libwg::uapi(to_c_char_array(buff));
        let result = CStr::from_ptr(s).to_bytes().to_vec();
        libc::free(s as *mut c_void);
        result
    }
}
```

**问题**: 
1. `to_c_char_array` 返回的指针从未被释放
2. `from_vec_unchecked` 没有验证是否包含 null 字节

**修复**:
```rust
fn to_c_string(data: &[u8]) -> Result<CString, std::ffi::NulError> {
    CString::new(data.to_vec())
}

fn uapi(buff: &[u8]) -> io::Result<Vec<u8>> {
    let c_str = to_c_string(buff)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    
    unsafe {
        let result_ptr = libwg::uapi(c_str.as_ptr());
        if result_ptr.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::Other, 
                "uapi call returned null"
            ));
        }
        
        let result = CStr::from_ptr(result_ptr).to_bytes().to_vec();
        libc::free(result_ptr as *mut c_void);
        Ok(result)
    }
}
```

然后更新所有调用点来处理错误:
```rust
// src/wg.rs 第 103 行
let data = uapi(buff.as_bytes())
    .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("uapi error: {}", e)))?;

// src/wg.rs 第 126 行  
let data = uapi(b"get=1\n\n")
    .map_err(|e| {
        log::error!("uapi error: {}", e);
        return;  // 或者适当处理
    })?;
```

---

## 🔴 安全问题

### 4. 密码以明文存储在内存中

**位置**: `src/config.rs` 第 33 行

**当前代码**:
```rust
pub password: Option<String>,
```

**修复**: 添加 secrecy crate 到 Cargo.toml:
```toml
secrecy = "0.8"
```

然后修改:
```rust
use secrecy::{Secret, ExposeSecret};

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    // ...
    #[serde(serialize_with = "serialize_secret")]
    #[serde(deserialize_with = "deserialize_secret")]
    pub password: Option<Secret<String>>,
    // ...
}

fn serialize_secret<S>(secret: &Option<Secret<String>>, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match secret {
        Some(secret) => s.serialize_some(secret.expose_secret()),
        None => s.serialize_none(),
    }
}

fn deserialize_secret<'de, D>(d: D) -> Result<Option<Secret<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(d)?;
    Ok(opt.map(Secret::new))
}
```

然后在使用时:
```rust
// src/client.rs 第 309 行
if let Some(password) = &self.conf.password {
    if !password.expose_secret().is_empty() {
        // ...
    }
}
```

---

### 5. Cookie 文件权限不安全

**位置**: `src/client.rs` 第 176-186 行

**当前代码**:
```rust
let mut file = fs::OpenOptions::new()
    .write(true)
    .create(true)
    .append(false)
    .open(format!(...))
    .map(io::BufWriter::new)
    .unwrap();
```

**修复**:
```rust
fn save_cookie(&self) -> io::Result<()> {
    let cookie_path = format!(
        "{}_{}",
        self.conf.interface_name.clone().unwrap(),
        COOKIE_FILE_SUFFIX
    );
    
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&cookie_path)
        .map(io::BufWriter::new)?;
    
    // 在 Unix 系统上设置权限为 600
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::metadata(&cookie_path)?;
        let mut perms = metadata.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&cookie_path, perms)?;
    }
    
    let c = self.cookie.lock()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    c.save_json(&mut file)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    
    Ok(())
}
```

---

## 🟡 高优先级改进

### 6. 过度使用 unwrap() 和 panic!()

需要系统性地将所有 `unwrap()` 替换为适当的错误处理。

**示例位置**: `src/client.rs`

**改进策略**:

1. 添加 `anyhow` 或 `thiserror` 到 Cargo.toml:
```toml
thiserror = "1.0"
```

2. 定义更好的错误类型:
```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ClientError {
    #[error("Network request failed: {0}")]
    Network(#[from] reqwest::Error),
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    
    #[error("Cookie error: {0}")]
    CookieError(String),
    
    #[error("Authentication failed: {reason}")]
    AuthenticationFailed { reason: String },
    
    #[error("VPN connection failed: {reason}")]
    VpnConnectionFailed { reason: String },
    
    #[error("Configuration error: {0}")]
    ConfigError(String),
    
    #[error("Invalid response: {0}")]
    InvalidResponse(String),
}
```

3. 替换示例:

**之前**:
```rust
let body = serde_json::to_string(&m).unwrap();
```

**之后**:
```rust
let body = serde_json::to_string(&m)?;
```

**之前**:
```rust
panic!("unsupported platform, please contact the developer");
```

**之后**:
```rust
return Err(ClientError::ConfigError(
    "unsupported platform".to_string()
));
```

---

### 7. 改进 Send/Sync 实现

**位置**: `src/client.rs` 第 59-61 行

**当前代码**:
```rust
unsafe impl Send for Client {}
unsafe impl Sync for Client {}
```

**修复**: 添加文档说明为什么这是安全的:
```rust
// SAFETY: Client 包含:
// - conf: Config - 所有字段都是 Send + Sync
// - cookie: Arc<CookieStoreMutex> - CookieStoreMutex 内部使用 Mutex，是 Send + Sync
// - c: reqwest::Client - reqwest::Client 本身实现了 Send + Sync
// - api_url: ApiUrl - 所有字段都是 Send + Sync  
// - date_offset_sec: i32 - 是 Copy 类型，天然 Send + Sync
//
// 因此 Client 可以安全地实现 Send 和 Sync
unsafe impl Send for Client {}
unsafe impl Sync for Client {}
```

或者更好的做法是验证这是否真的需要：
```rust
// 如果所有字段都已经是 Send + Sync，编译器会自动推导，
// 无需手动实现。检查是否可以移除这些实现。
```

---

### 8. 配置文件保存时的错误处理

**位置**: `src/config.rs` 第 109-113 行

**当前代码**:
```rust
pub async fn save(&self) {
    let file = self.conf_file.as_ref().unwrap();
    let data = format!("{}", &self);
    fs::write(file, data).await.unwrap();
}
```

**修复**:
```rust
pub async fn save(&self) -> io::Result<()> {
    let file = self.conf_file.as_ref()
        .ok_or_else(|| io::Error::new(
            io::ErrorKind::NotFound, 
            "config file path not set"
        ))?;
    
    let data = serde_json::to_string_pretty(self)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    
    // 使用临时文件 + 原子重命名来防止部分写入
    let temp_file = format!("{}.tmp", file);
    fs::write(&temp_file, data).await?;
    fs::rename(&temp_file, file).await?;
    
    // 在 Unix 上设置权限
    #[cfg(unix)]
    {
        use tokio::fs::metadata;
        use std::os::unix::fs::PermissionsExt;
        
        let metadata = metadata(file).await?;
        let mut perms = metadata.permissions();
        perms.set_mode(0o600);
        tokio::fs::set_permissions(file, perms).await?;
    }
    
    Ok(())
}
```

---

## 🟡 中优先级改进

### 9. 减少不必要的字符串分配

**位置**: `src/wg.rs` 第 62-99 行

**当前代码**:
```rust
let mut buff = String::from("set=1\n");
buff.push_str(format!("private_key={private_key}\n").as_str());
buff.push_str("replace_peers=true\n".to_string().as_str());
```

**修复**:
```rust
use std::fmt::Write;

let mut buff = String::with_capacity(1024);  // 预分配合理大小
writeln!(buff, "set=1")?;
writeln!(buff, "private_key={}", private_key)?;
writeln!(buff, "replace_peers=true")?;
writeln!(buff, "public_key={}", public_key)?;
writeln!(buff, "replace_allowed_ips=true")?;
writeln!(buff, "endpoint={}", conf.peer_address)?;
writeln!(buff, "persistent_keepalive_interval=10")?;

for route in &conf.route {
    if route.contains('/') {
        writeln!(buff, "allowed_ip={}", route)?;
    } else {
        writeln!(buff, "allowed_ip={}/32", route)?;
    }
}

writeln!(buff, "address={}", conf.address)?;
if !conf.address6.is_empty() {
    writeln!(buff, "address={}", conf.address6)?;
}
writeln!(buff, "mtu={}", conf.mtu)?;
writeln!(buff, "up=true")?;

for route in &conf.route {
    if route.contains('/') {
        writeln!(buff, "route={}", route)?;
    } else {
        let prefix_len = if route.contains(':') { 128 } else { 32 };
        writeln!(buff, "route={}/{}", route, prefix_len)?;
    }
}

buff.push('\n');
```

---

### 10. 添加基本的单元测试

创建 `src/utils.rs` 测试:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gen_wg_keypair() {
        let (public, private) = gen_wg_keypair();
        
        // Base64 编码的 32 字节密钥应该是 44 字符（含填充）
        assert_eq!(public.len(), 44);
        assert_eq!(private.len(), 44);
        
        // 验证可以从私钥生成公钥
        let derived = gen_public_key_from_private(&private)
            .expect("should generate public key from private");
        assert_eq!(derived, public);
    }

    #[test]
    fn test_gen_public_key_from_private_invalid() {
        let result = gen_public_key_from_private(&"not-valid-base64".to_string());
        assert!(result.is_err());
    }

    #[test]
    fn test_b64_decode_to_hex() {
        let input = "SGVsbG8=";  // "Hello" 的 Base64
        let output = b64_decode_to_hex(input);
        assert_eq!(output, "48656c6c6f");
    }

    #[test]
    fn test_b32_decode() {
        let input = "JBSWY3DPEBLW64TMMQ======";  // "Hello" 的 Base32
        let output = b32_decode(input);
        assert_eq!(output, b"Hello");
    }
}
```

创建 `src/totp.rs` 测试:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hotp() {
        // RFC 4226 测试向量
        let secret = b"12345678901234567890";
        
        assert_eq!(hotp(secret, 0, 6), 755224);
        assert_eq!(hotp(secret, 1, 6), 287082);
        assert_eq!(hotp(secret, 2, 6), 359152);
    }

    #[test]
    fn test_totp_slot() {
        let secret = b"12345678901234567890";
        let slot = totp_offset(secret, 0);
        
        // 验证生成的代码是 6 位数
        assert!(slot.code < 1_000_000);
        
        // 验证剩余秒数在合理范围内
        assert!(slot.secs_left > 0 && slot.secs_left <= TIME_STEP as u32);
    }
}
```

---

## 实施建议

1. **第一优先级**: 修复 1-5（严重 bug 和内存/安全问题）
2. **第二优先级**: 修复 6-8（错误处理）
3. **第三优先级**: 修复 9-10（性能和测试）

每个修复都应该：
1. 创建新分支
2. 实现修复
3. 添加测试（如果适用）
4. 提交 PR 进行审查
5. 合并到主分支

## 验证清单

- [ ] 修复 State::Display 无限递归
- [ ] 修复 "ctrl+v" 日志消息
- [ ] 修复 FFI 内存泄漏
- [ ] 实现密码安全存储
- [ ] 设置 Cookie 文件权限
- [ ] 改进错误处理（移除 unwrap/panic）
- [ ] 添加 Send/Sync 文档
- [ ] 改进配置保存
- [ ] 优化字符串操作
- [ ] 添加单元测试

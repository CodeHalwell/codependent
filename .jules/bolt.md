## 2023-10-27 - Table detection parsing optimization
**Learning:** Checking if an ASCII character (like `-`) appears in a string by using `.chars()` incurs Unicode decoding overhead.
**Action:** Use `.as_bytes().iter().all(|&c| c == b'-')` instead of `.chars().all(|c| c == '-')` for pure ASCII checks to skip UTF-8 processing, resulting in significant performance improvements.
## 2024-05-18 - Safe ASCII String Validation
**Learning:** When validating purely ASCII strings (like hex IDs and slugs), avoiding `.chars()` and instead using `.as_bytes().iter()` bypasses unnecessary UTF-8 decoding overhead. Even better, since any invalid multi-byte character will fail the ASCII check on its first byte, the failing index is guaranteed to be a valid char boundary.
**Action:** Use `.as_bytes().iter().enumerate()` for fast ASCII string validation, and safely extract the exact offending char for error messages using `s[i..].chars().next().unwrap()`.

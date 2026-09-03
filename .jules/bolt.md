## 2023-10-27 - Table detection parsing optimization
**Learning:** Checking if an ASCII character (like `-`) appears in a string by using `.chars()` incurs Unicode decoding overhead.
**Action:** Use `.as_bytes().iter().all(|&c| c == b'-')` instead of `.chars().all(|c| c == '-')` for pure ASCII checks to skip UTF-8 processing, resulting in significant performance improvements.

## 2024-05-18 - [Optimization] Avoid .chars() for pure ASCII string validation
**Learning:** In Rust, `.chars()` decodes UTF-8 which adds unnecessary overhead when validating strings that are expected to contain only pure ASCII characters (like slugs, hex codes, or specific delimiters).
**Action:** Replace `.chars()` with `.as_bytes().iter().enumerate()` (or `.bytes()`) when checking string contents against purely ASCII constraints. If an error needs to report the specific character, use the index from `enumerate()` and safely extract the character with `s[i..].chars().next().unwrap()`, since the first non-ASCII byte is guaranteed to be a valid character boundary.

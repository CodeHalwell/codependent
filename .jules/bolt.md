## 2023-10-27 - Table detection parsing optimization
**Learning:** Checking if an ASCII character (like `-`) appears in a string by using `.chars()` incurs Unicode decoding overhead.
**Action:** Use `.as_bytes().iter().all(|&c| c == b'-')` instead of `.chars().all(|c| c == '-')` for pure ASCII checks to skip UTF-8 processing, resulting in significant performance improvements.
## 2024-11-20 - Preserving Error Precision with .as_bytes()
**Learning:** Using `.as_bytes().iter()` for ASCII validation is fast, but just casting the first invalid byte to a `char` (e.g., `b as char`) produces garbled error messages when a multi-byte Unicode sequence is hit.
**Action:** When validating ASCII on strings using bytes to avoid UTF-8 decoding overhead, find the index of the failure and safely extract the original `char` using `s[i..].chars().next().unwrap()` to maintain accurate error reporting without degrading the hot-path speed.

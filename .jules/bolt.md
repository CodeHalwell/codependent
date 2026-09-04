## 2023-10-27 - Table detection parsing optimization
**Learning:** Checking if an ASCII character (like `-`) appears in a string by using `.chars()` incurs Unicode decoding overhead.
**Action:** Use `.as_bytes().iter().all(|&c| c == b'-')` instead of `.chars().all(|c| c == '-')` for pure ASCII checks to skip UTF-8 processing, resulting in significant performance improvements.
## 2024-11-20 - Skip UTF-8 Decoding Overhead for Pure ASCII Checks in Rust
**Learning:** Using `.chars()` in Rust implicitly decodes UTF-8, which adds unnecessary overhead when dealing with strings guaranteed or expected to be ASCII (e.g., hex hashes, URL slugs).
**Action:** Replace `.chars()` loops with `.as_bytes().iter().enumerate()` for validation logic that checks against pure ASCII bytes (using `b'-'` instead of `'-'`). If an invalid byte is found, the index `i` can safely be used to slice the string and get the offending character with `s[i..].chars().next().unwrap()` to construct accurate errors without penalizing the happy path.

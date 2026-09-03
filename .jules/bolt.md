## 2023-10-27 - Table detection parsing optimization
**Learning:** Checking if an ASCII character (like `-`) appears in a string by using `.chars()` incurs Unicode decoding overhead.
**Action:** Use `.as_bytes().iter().all(|&c| c == b'-')` instead of `.chars().all(|c| c == '-')` for pure ASCII checks to skip UTF-8 processing, resulting in significant performance improvements.
## 2024-05-18 - Avoid UTF-8 decoding overhead for pure ASCII validation
**Learning:** Pure ASCII validations (like hex strings and slugs) can avoid the overhead of `s.chars()` which does full UTF-8 decoding. Iterating over `s.as_bytes()` directly is faster. The index `i` of the first invalid byte is guaranteed to be a valid char boundary, so extracting the offending character on the error path is safe.
**Action:** Use `.as_bytes().iter().enumerate()` instead of `.chars()` for pure ASCII string validations to avoid UTF-8 decoding overhead on the happy path.

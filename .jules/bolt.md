## 2023-10-27 - Table detection parsing optimization
**Learning:** Checking if an ASCII character (like `-`) appears in a string by using `.chars()` incurs Unicode decoding overhead.
**Action:** Use `.as_bytes().iter().all(|&c| c == b'-')` instead of `.chars().all(|c| c == '-')` for pure ASCII checks to skip UTF-8 processing, resulting in significant performance improvements.
## 2024-08-27 - Optimize ASCII String Validation
**Learning:** Using `.chars().all(...)` for pure ASCII string validation incurs unnecessary UTF-8 decoding overhead. For constraints involving only ASCII characters (like letters, digits, and specific punctuation), analyzing the string as bytes is significantly faster.
**Action:** Use `.bytes().all(...)` (or `&s.as_bytes()[..].iter().all(...)`) and compare against byte literals (e.g., `b'-'`) instead of `.chars().all(...)` when enforcing pure ASCII boundaries. This skips UTF-8 processing while correctly applying the constraint.

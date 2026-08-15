//! The command-arity dictionary (adoption 07), ported from opencode's
//! `permission/arity.ts`: how many leading tokens make a shell command
//! "human-understandable", so an "always allow" learns `git checkout *`
//! rather than the literal invocation.
//!
//! Port-faithful semantics: the lookup slices RAW tokens — flags are excluded
//! from the dictionary's counts by construction, not skipped at match time —
//! and the longest listed prefix wins. Unknown program ⇒ the program token
//! alone (the conservative default).

/// `(prefix, arity)` pairs, sorted by prefix for the binary search below.
/// Verbatim port of opencode's generated dictionary (entries whose programs
/// can never pass codypendent's allow-list are still kept: the dictionary is
/// data about commands, the allow-list is policy about them).
const ARITY: &[(&str, usize)] = &[
    ("aws", 3),
    ("az", 3),
    ("bazel", 2),
    ("brew", 2),
    ("bun", 2),
    ("bun run", 3),
    ("bun x", 3),
    ("cargo", 2),
    ("cargo add", 3),
    ("cargo run", 3),
    ("cat", 1),
    ("cd", 1),
    ("cdk", 2),
    ("cf", 2),
    ("chmod", 1),
    ("chown", 1),
    ("cmake", 2),
    ("composer", 2),
    ("consul", 2),
    ("consul kv", 3),
    ("cp", 1),
    ("crictl", 2),
    ("deno", 2),
    ("deno task", 3),
    ("docker", 2),
    ("docker builder", 3),
    ("docker compose", 3),
    ("docker container", 3),
    ("docker image", 3),
    ("docker network", 3),
    ("docker volume", 3),
    ("doctl", 3),
    ("echo", 1),
    ("eksctl", 2),
    ("eksctl create", 3),
    ("env", 1),
    ("export", 1),
    ("firebase", 2),
    ("flyctl", 2),
    ("gcloud", 3),
    ("gh", 3),
    ("git", 2),
    ("git config", 3),
    ("git remote", 3),
    ("git stash", 3),
    ("go", 2),
    ("gradle", 2),
    ("grep", 1),
    ("helm", 2),
    ("heroku", 2),
    ("hugo", 2),
    ("ip", 2),
    ("ip addr", 3),
    ("ip link", 3),
    ("ip netns", 3),
    ("ip route", 3),
    ("kill", 1),
    ("killall", 1),
    ("kind", 2),
    ("kind create", 3),
    ("kubectl", 2),
    ("kubectl kustomize", 3),
    ("kubectl rollout", 3),
    ("kustomize", 2),
    ("ln", 1),
    ("ls", 1),
    ("make", 2),
    ("mc", 2),
    ("mc admin", 3),
    ("minikube", 2),
    ("mkdir", 1),
    ("mongosh", 2),
    ("mv", 1),
    ("mvn", 2),
    ("mysql", 2),
    ("ng", 2),
    ("npm", 2),
    ("npm exec", 3),
    ("npm init", 3),
    ("npm run", 3),
    ("npm view", 3),
    ("nvm", 2),
    ("nx", 2),
    ("openssl", 2),
    ("openssl req", 3),
    ("openssl x509", 3),
    ("pip", 2),
    ("pipenv", 2),
    ("pnpm", 2),
    ("pnpm dlx", 3),
    ("pnpm exec", 3),
    ("pnpm run", 3),
    ("podman", 2),
    ("podman container", 3),
    ("podman image", 3),
    ("poetry", 2),
    ("ps", 1),
    ("psql", 2),
    ("pulumi", 2),
    ("pulumi stack", 3),
    ("pwd", 1),
    ("pyenv", 2),
    ("python", 2),
    ("rake", 2),
    ("rbenv", 2),
    ("redis-cli", 2),
    ("rm", 1),
    ("rmdir", 1),
    ("rustup", 2),
    ("serverless", 2),
    ("sfdx", 3),
    ("skaffold", 2),
    ("sleep", 1),
    ("sls", 2),
    ("source", 1),
    ("sst", 2),
    ("swift", 2),
    ("systemctl", 2),
    ("tail", 1),
    ("terraform", 2),
    ("terraform workspace", 3),
    ("tmux", 2),
    ("touch", 1),
    ("turbo", 2),
    ("ufw", 2),
    ("unset", 1),
    ("vault", 2),
    ("vault auth", 3),
    ("vault kv", 3),
    ("vercel", 2),
    ("volta", 2),
    ("which", 1),
    ("wp", 2),
    ("yarn", 2),
    ("yarn dlx", 3),
    ("yarn run", 3),
];

fn lookup(prefix: &str) -> Option<usize> {
    ARITY
        .binary_search_by(|(p, _)| p.cmp(&prefix))
        .ok()
        .map(|i| ARITY[i].1)
}

/// The human-understandable prefix of `tokens` (port of `BashArity.prefix`):
/// longest listed prefix wins; unknown ⇒ the first token; empty ⇒ empty.
#[must_use]
pub fn command_prefix(tokens: &[String]) -> Vec<String> {
    for len in (1..=tokens.len()).rev() {
        let candidate = tokens[..len].join(" ");
        if let Some(arity) = lookup(&candidate) {
            return tokens[..arity.min(tokens.len())].to_vec();
        }
    }
    tokens.first().cloned().into_iter().collect()
}

/// Programs whose arguments ARE programs/scripts: a learned prefix would be a
/// blank check (`sh *`, `python *`), so these never produce a pattern. Closed
/// list; extending it only ever narrows.
pub const UNLEARNABLE_PROGRAMS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "dash",
    "fish",
    "ksh",
    "pwsh",
    "powershell",
    "python",
    "python3",
    "node",
    "deno",
    "bun",
    "ruby",
    "perl",
    "php",
    "env",
    "xargs",
    "sudo",
    "doas",
    "nice",
    "nohup",
    "time",
    "timeout",
    "eval",
];

/// The `always allow` pattern for an ExecuteCommand, or `None` when learning
/// is refused. Refusals (each a RULE in §7):
/// - program not a bare name (path separators ⇒ pinned binary, no pattern);
/// - program in [`UNLEARNABLE_PROGRAMS`];
/// - non-empty `environment` (a pattern learned from a clean env must never
///   auto-approve a call that adds variables — the `ExecuteCommand.environment`
///   smuggling channel);
/// - empty token list.
#[must_use]
pub fn command_pattern(
    program: &str,
    args: &[String],
    environment: &[(String, String)],
) -> Option<String> {
    if program.is_empty()
        || program.contains('/')
        || program.contains('\\')
        || !environment.is_empty()
        || UNLEARNABLE_PROGRAMS.contains(&program)
    {
        return None;
    }
    let mut tokens = Vec::with_capacity(args.len() + 1);
    tokens.push(program.to_string());
    tokens.extend(args.iter().cloned());
    let prefix = command_prefix(&tokens);
    // A prefix that contains a flag token cannot be safely generalized: the flag
    // changes what the trailing `*` means, so `git -c http.proxy=x fetch` would
    // learn `git -c *` and then auto-approve `git -c core.sshCommand=/tmp/pwn
    // push` — arbitrary code behind two matched leading tokens. Any flag anywhere
    // in the learned span (not just git's `-c`) makes the pattern a blank check,
    // so refuse to learn one at all.
    if prefix.iter().any(|token| token.starts_with('-')) {
        return None;
    }
    Some(format!("{} *", prefix.join(" ")))
}

/// Whether `pattern` (from [`command_pattern`]) covers this invocation:
/// the pattern's tokens (sans the trailing `*`) must equal the invocation's
/// leading tokens exactly. No globbing inside tokens — `*` is only ever the
/// whole tail.
#[must_use]
pub fn pattern_matches(pattern: &str, program: &str, args: &[String]) -> bool {
    let Some(head) = pattern.strip_suffix(" *") else {
        return false;
    };
    let want: Vec<&str> = head.split(' ').collect();
    let mut have = vec![program];
    have.extend(args.iter().map(String::as_str));
    have.len() >= want.len() && have[..want.len()] == want[..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionary_is_sorted_for_binary_search() {
        assert!(
            ARITY.windows(2).all(|w| w[0].0 < w[1].0),
            "ARITY table must be sorted lexicographically"
        );
    }

    #[test]
    fn prefix_matches_reference_examples() {
        let t1 = vec!["touch".to_string(), "foo.txt".to_string()];
        assert_eq!(command_prefix(&t1), vec!["touch"]);

        let t2 = vec![
            "git".to_string(),
            "checkout".to_string(),
            "main".to_string(),
        ];
        assert_eq!(command_prefix(&t2), vec!["git", "checkout"]);

        let t3 = vec!["npm".to_string(), "run".to_string(), "dev".to_string()];
        assert_eq!(command_prefix(&t3), vec!["npm", "run", "dev"]);

        let t4 = vec!["python".to_string(), "script.py".to_string()];
        assert_eq!(command_prefix(&t4), vec!["python", "script.py"]);
    }

    #[test]
    fn longest_prefix_wins() {
        let tokens = vec![
            "cargo".to_string(),
            "run".to_string(),
            "my-bin".to_string(),
            "--flag".to_string(),
        ];
        // "cargo" is in ARITY (2), but "cargo run" is in ARITY (3) -> longest prefix wins
        assert_eq!(command_prefix(&tokens), vec!["cargo", "run", "my-bin"]);
    }

    #[test]
    fn unknown_program_slices_one() {
        let tokens = vec!["mycustomtool".to_string(), "subcmd".to_string()];
        assert_eq!(command_prefix(&tokens), vec!["mycustomtool"]);
    }

    #[test]
    fn pattern_refuses_interpreters_paths_and_env() {
        let clean = Vec::new();
        let with_env = vec![("FOO".to_string(), "bar".to_string())];
        let args = vec!["checkout".to_string(), "main".to_string()];

        // Bare git is learnable
        assert_eq!(
            command_pattern("git", &args, &clean),
            Some("git checkout *".to_string())
        );

        // Pinned path rejected
        assert_eq!(command_pattern("/usr/bin/git", &args, &clean), None);
        assert_eq!(command_pattern("git\\sub", &args, &clean), None);

        // Env presence rejected
        assert_eq!(command_pattern("git", &args, &with_env), None);

        // Unlearnable interpreters rejected
        assert_eq!(command_pattern("sh", &args, &clean), None);
        assert_eq!(command_pattern("bash", &args, &clean), None);
        assert_eq!(command_pattern("python", &args, &clean), None);
        assert_eq!(command_pattern("node", &args, &clean), None);
    }

    #[test]
    fn flag_valued_prefix_is_not_learnable() {
        let clean = Vec::new();

        // `git -c http.proxy=x fetch` learns prefix ["git", "-c"] under git's
        // arity of 2. That would produce `git -c *`, which auto-approves
        // `git -c core.sshCommand=/tmp/pwn push` — arbitrary code behind two
        // matched leading tokens. A flag in the learned span must refuse to learn.
        let git_c = vec![
            "-c".to_string(),
            "http.proxy=x".to_string(),
            "fetch".to_string(),
        ];
        assert_eq!(command_pattern("git", &git_c, &clean), None);

        // And the malicious follow-up must not be coverable by any learned git
        // pattern derived from the flag form.
        assert!(command_pattern("git", &git_c, &clean).is_none());

        // A flag as the program's own leading token is likewise unlearnable.
        let leading_flag = vec!["-e".to_string(), "code".to_string()];
        assert_eq!(command_pattern("git", &leading_flag, &clean), None);

        // Normal structured invocations still learn their reference pattern.
        assert_eq!(
            command_pattern(
                "git",
                &["checkout".to_string(), "feature-x".to_string()],
                &clean
            ),
            Some("git checkout *".to_string())
        );
    }

    #[test]
    fn pattern_matches_is_token_exact() {
        let pattern = "git checkout *";
        let args_sub = vec!["-b".to_string(), "feature".to_string()];
        assert!(pattern_matches(pattern, "git", &["checkout".to_string()]));
        let mut full_args = vec!["checkout".to_string()];
        full_args.extend(args_sub);
        assert!(pattern_matches(pattern, "git", &full_args));

        // Unrelated subcommand
        assert!(!pattern_matches(
            pattern,
            "git",
            &["commit".to_string(), "-m".to_string(), "msg".to_string()]
        ));

        // Substring prefix that is not token-separated
        assert!(!pattern_matches(
            pattern,
            "git",
            &["checkoutfoo".to_string()]
        ));
    }
}

import React from "react";

/**
 * Shown while the authorization code is being exchanged for a session.
 *
 * Without it the provider's redirect lands on the login screen for as long as
 * the exchange takes, which reads as "sign-in failed" at the exact moment it
 * is succeeding.
 */
export function CallbackView({ error }: { error?: string | undefined }): React.JSX.Element {
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        minHeight: "100vh",
        padding: "20px",
      }}
      data-testid="callback-view"
    >
      <div
        className="card"
        style={{ width: "100%", maxWidth: "420px", padding: "32px 24px", textAlign: "center" }}
      >
        {error ? (
          <>
            <h1 style={{ fontSize: "18px", fontWeight: 700 }}>Sign-in didn&rsquo;t complete</h1>
            <p
              style={{ fontSize: "13px", color: "var(--text-secondary)", marginTop: "8px" }}
              data-testid="callback-error"
            >
              {error}
            </p>
            <button
              className="btn btn-primary"
              style={{ marginTop: "20px", padding: "10px 16px" }}
              onClick={() => {
                window.location.hash = "#login";
              }}
              data-testid="callback-retry-btn"
            >
              Back to sign in
            </button>
          </>
        ) : (
          <>
            <h1 style={{ fontSize: "18px", fontWeight: 700 }}>Completing sign-in&hellip;</h1>
            <p style={{ fontSize: "13px", color: "var(--text-secondary)", marginTop: "8px" }}>
              Exchanging your authorization code for a session.
            </p>
          </>
        )}
      </div>
    </div>
  );
}

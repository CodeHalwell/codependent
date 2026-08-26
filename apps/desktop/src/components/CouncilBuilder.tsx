import React, { useState } from "react";
import type { CouncilCard, CouncilDraft, CouncilMemberRow } from "../localConfig.js";

/**
 * Building a new council.
 *
 * Deliberately collects exactly what the TUI's wizard collects — name,
 * description, chair, rounds, members — and nothing more. The TUI has no quorum
 * step and no evidence step, and passes `quorum: None, evidence: false`; adding
 * either here would invent a control the rest of the product does not have.
 *
 * **Validation is not duplicated here.** The council crate owns it: the name
 * charset (it becomes a directory under `<data>/councils/`), the 2..=N member
 * bound, unique member models, and — the one that catches people — every member
 * model AND the chair having to already exist in `models.toml`. Those refusals
 * are shown verbatim, so the desktop cannot accept a council the TUI would
 * reject, or reject one it would accept. The only checks made locally are the
 * ones needed to keep the form from submitting something obviously incomplete.
 */
export interface CouncilBuilderProps {
  onCreate: (draft: CouncilDraft) => Promise<CouncilCard>;
  onCancel?: () => void;
  /** Called once a council was actually persisted. */
  onCreated?: (council: CouncilCard) => void;
  /**
   * Model ids that are configured, when the caller knows them, so the form can
   * offer them as suggestions. Absent means we do not know — the field stays
   * free text and the crate's refusal is the authority either way.
   */
  configuredModels?: string[];
}

export const CouncilBuilder: React.FC<CouncilBuilderProps> = ({
  onCreate,
  onCancel,
  onCreated,
  configuredModels,
}) => {
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [chair, setChair] = useState("");
  const [rounds, setRounds] = useState(1);
  const [members, setMembers] = useState<CouncilMemberRow[]>([
    { model: "", role: "member" },
    { model: "", role: "member" },
  ]);
  const [refusal, setRefusal] = useState<string | null>(null);
  const [created, setCreated] = useState<CouncilCard | null>(null);
  const [busy, setBusy] = useState(false);

  const updateMember = (index: number, patch: Partial<CouncilMemberRow>) => {
    setMembers((current) =>
      current.map((member, position) => (position === index ? { ...member, ...patch } : member)),
    );
  };

  const filled = members.filter((member) => member.model.trim().length > 0);
  // Only the checks that stop an obviously incomplete submission. Everything
  // else — including whether these models exist — is the crate's to answer.
  const submittable =
    name.trim().length > 0 && chair.trim().length > 0 && filled.length >= 2 && !busy;
  // The FIRST unmet condition, so a disabled Create button explains itself
  // instead of leaving the operator to diff the form against the rules.
  const blockedBecause = busy
    ? null
    : name.trim().length === 0
      ? "a name is required"
      : filled.length < 2
        ? `at least two members need a model (${filled.length} of 2 so far)`
        : chair.trim().length === 0
          ? "a chair model is required"
          : null;

  const submit = async () => {
    setRefusal(null);
    setCreated(null);
    setBusy(true);
    try {
      const council = await onCreate({
        name: name.trim(),
        description: description.trim(),
        chair: chair.trim(),
        rounds,
        members: filled.map((member) => ({
          model: member.model.trim(),
          role: member.role.trim() || "member",
        })),
      });
      setCreated(council);
      onCreated?.(council);
    } catch (error) {
      setRefusal(describe(error));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div style={{ padding: 24, overflowY: "auto", color: "#e6edf3", maxWidth: 760 }}>
      <h2 style={{ margin: "0 0 4px", fontSize: 18, fontWeight: 600 }}>New council</h2>
      <p style={{ margin: "0 0 20px", fontSize: 13, color: "#8b949e" }}>
        Two or more members deliberate; the chair synthesizes. Every model named here must
        already be configured in <code>models.toml</code> — the council is refused otherwise
        rather than created and then failing at run time.
      </p>

      <Labelled label="Name" hint="Becomes a directory under the council report store, so letters, digits, dot, underscore and dash only.">
        <input
          data-testid="council-name"
          value={name}
          onChange={(event) => setName(event.target.value)}
          style={inputStyle}
        />
      </Labelled>

      <Labelled label="Description" hint="Optional.">
        <input
          data-testid="council-description"
          value={description}
          onChange={(event) => setDescription(event.target.value)}
          style={inputStyle}
        />
      </Labelled>

      <Labelled label="Chair" hint="The model that synthesizes. May also sit as a member, but it then weighs its own report.">
        <input
          data-testid="council-chair"
          value={chair}
          onChange={(event) => setChair(event.target.value)}
          list={configuredModels ? "council-model-choices" : undefined}
          style={inputStyle}
        />
      </Labelled>

      <Labelled label="Rounds" hint="Each round re-runs every member with the previous round's dossier.">
        <input
          data-testid="council-rounds"
          type="number"
          min={1}
          value={rounds}
          onChange={(event) => setRounds(Math.max(1, Number(event.target.value) || 1))}
          style={{ ...inputStyle, width: 100 }}
        />
      </Labelled>

      {configuredModels && (
        <datalist id="council-model-choices">
          {configuredModels.map((model) => (
            <option key={model} value={model} />
          ))}
        </datalist>
      )}

      <div style={{ marginTop: 20 }}>
        <div style={{ fontSize: 13, fontWeight: 600, marginBottom: 4 }}>Members</div>
        <div style={{ fontSize: 12, color: "#8b949e", marginBottom: 8 }}>
          At least two, and no model may appear twice.
        </div>
        {members.map((member, index) => (
          <div key={index} style={{ display: "flex", gap: 8, marginBottom: 8 }}>
            <input
              data-testid={`council-member-model-${index}`}
              value={member.model}
              onChange={(event) => updateMember(index, { model: event.target.value })}
              placeholder="model id"
              list={configuredModels ? "council-model-choices" : undefined}
              style={{ ...inputStyle, flex: 2 }}
            />
            <input
              data-testid={`council-member-role-${index}`}
              value={member.role}
              onChange={(event) => updateMember(index, { role: event.target.value })}
              placeholder="role"
              style={{ ...inputStyle, flex: 1 }}
            />
            <button
              onClick={() => setMembers((current) => current.filter((_, position) => position !== index))}
              disabled={members.length <= 2}
              style={buttonStyle}
              data-testid={`council-member-remove-${index}`}
              aria-label={`Remove member ${index + 1}`}
            >
              Remove
            </button>
          </div>
        ))}
        <button
          onClick={() => setMembers((current) => [...current, { model: "", role: "member" }])}
          style={buttonStyle}
          data-testid="council-member-add"
        >
          Add member
        </button>
      </div>

      {refusal && (
        /* The council crate's own words. Rewriting them would let the desktop
           and the TUI disagree about why something was refused. */
        <div role="alert" data-testid="council-builder-refusal" style={refusalStyle}>
          {refusal}
        </div>
      )}

      {created && (
        <div role="status" data-testid="council-builder-created" style={createdStyle}>
          Created “{created.name}” — {created.members.length} members, chair{" "}
          <code>{created.chair}</code>, {created.rounds} round(s), quorum{" "}
          {created.requiredQuorum}.
          {created.chairIsMember && (
            <div style={{ marginTop: 6, color: "#d29922" }}>
              The chair is also a member, so its synthesis will weigh its own report.
            </div>
          )}
        </div>
      )}

      <div style={{ display: "flex", gap: 8, marginTop: 20, alignItems: "center" }}>
        <button onClick={() => void submit()} disabled={!submittable} style={primaryButtonStyle} data-testid="council-builder-submit">
          Create council
        </button>
        {blockedBecause && (
          <span style={{ fontSize: 12, color: "#8b949e" }} data-testid="council-builder-blocked">
            {blockedBecause}
          </span>
        )}
        {onCancel && (
          <button onClick={onCancel} disabled={busy} style={buttonStyle} data-testid="council-builder-cancel">
            Cancel
          </button>
        )}
      </div>
    </div>
  );
};

const Labelled: React.FC<{ label: string; hint?: string; children: React.ReactNode }> = ({
  label,
  hint,
  children,
}) => (
  <label style={{ display: "block", marginTop: 16 }}>
    <div style={{ fontSize: 13, fontWeight: 600, marginBottom: 4 }}>{label}</div>
    {hint && <div style={{ fontSize: 12, color: "#8b949e", marginBottom: 6 }}>{hint}</div>}
    {children}
  </label>
);

const inputStyle: React.CSSProperties = {
  width: "100%",
  padding: "6px 10px",
  borderRadius: 6,
  border: "1px solid #30363d",
  background: "#0d1117",
  color: "#e6edf3",
  fontSize: 13,
};

const buttonStyle: React.CSSProperties = {
  padding: "6px 12px",
  borderRadius: 6,
  border: "1px solid #30363d",
  background: "#21262d",
  color: "#e6edf3",
  fontSize: 13,
  cursor: "pointer",
};

const primaryButtonStyle: React.CSSProperties = {
  ...buttonStyle,
  background: "#238636",
  borderColor: "#2ea043",
};

const refusalStyle: React.CSSProperties = {
  marginTop: 16,
  padding: 12,
  borderRadius: 8,
  border: "1px solid #da3633",
  background: "#2d1214",
  color: "#ffa198",
  fontSize: 13,
  whiteSpace: "pre-wrap",
};

const createdStyle: React.CSSProperties = {
  marginTop: 16,
  padding: 12,
  borderRadius: 8,
  border: "1px solid #2ea043",
  background: "#122619",
  color: "#7ee787",
  fontSize: 13,
};

function describe(error: unknown): string {
  if (typeof error === "string") {
    return error;
  }
  if (error instanceof Error) {
    return error.message;
  }
  return String(error);
}

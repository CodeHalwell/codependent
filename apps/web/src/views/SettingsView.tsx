import React, { useState, useEffect } from "react";
import { useOrganizations } from "@codypendent/control-plane-react";
import { Save, ShieldAlert, Check } from "lucide-react";
import type { PublicationClass } from "@codypendent/control-plane";

export function SettingsView(): React.JSX.Element {
  const { activeOrganization, updateOrganizationPolicy, isLoading } = useOrganizations();

  const [displayName, setDisplayName] = useState(activeOrganization?.displayName ?? "");
  const [maxPublicationClass, setMaxPublicationClass] = useState<PublicationClass>(
    activeOrganization?.maxPublicationClass ?? "metadata-shared"
  );
  const [retentionDays, setRetentionDays] = useState<number | undefined>(
    activeOrganization?.retentionDays ?? undefined
  );
  const [dataResidency, setDataResidency] = useState<string>(activeOrganization?.dataResidency ?? "");

  const [savedSuccess, setSavedSuccess] = useState(false);

  useEffect(() => {
    if (activeOrganization) {
      setDisplayName(activeOrganization.displayName);
      setMaxPublicationClass(activeOrganization.maxPublicationClass);
      setRetentionDays(activeOrganization.retentionDays ?? undefined);
      setDataResidency(activeOrganization.dataResidency ?? "");
    }
  }, [activeOrganization]);

  const handleSave = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!activeOrganization) return;

    await updateOrganizationPolicy(activeOrganization.id, {
      displayName,
      maxPublicationClass,
      retentionDays: retentionDays || null,
      dataResidency: dataResidency || null,
    });

    setSavedSuccess(true);
    setTimeout(() => setSavedSuccess(false), 3000);
  };

  if (!activeOrganization) {
    return (
      <div>
        <h1 className="page-title">Settings</h1>
        <p className="page-description">No active organization selected.</p>
      </div>
    );
  }

  return (
    <div data-testid="settings-view">
      <div className="page-header">
        <div>
          <h1 className="page-title">Organization Policy & Settings</h1>
          <p className="page-description">
            Configure organization-wide publication ceilings, data retention, and residency policies.
          </p>
        </div>
      </div>

      <form onSubmit={handleSave} style={{ maxWidth: "600px" }}>
        <div className="card">
          <h2 className="card-title" style={{ marginBottom: "16px" }}>General Info</h2>
          <div className="form-group">
            <label className="form-label">Organization Slug</label>
            <input className="form-input" value={activeOrganization.slug} disabled readOnly />
          </div>
          <div className="form-group">
            <label className="form-label">Display Name</label>
            <input
              className="form-input"
              value={displayName}
              onChange={(e) => setDisplayName(e.target.value)}
              required
              data-testid="org-name-input"
            />
          </div>
        </div>

        <div className="card">
          <h2 className="card-title" style={{ marginBottom: "8px" }}>Publication & Security Policy</h2>
          <div className="privacy-banner" style={{ marginBottom: "16px" }}>
            <div style={{ display: "flex", alignItems: "center", gap: "6px", marginBottom: "4px" }}>
              <ShieldAlert size={14} color="var(--warning)" />
              <strong>Downwards-Only Narrowing Ceiling</strong>
            </div>
            <p>
              The organization publication ceiling sets the upper limit for all repositories and workstations.
              A repository cannot publish higher than this ceiling, and daemons always intersect with their local policy.
            </p>
          </div>

          <div className="form-group">
            <label className="form-label">Maximum Publication Class</label>
            <select
              className="form-select"
              value={maxPublicationClass}
              onChange={(e) => setMaxPublicationClass(e.target.value as PublicationClass)}
              data-testid="max-pub-class-select"
            >
              <option value="private-local">Private Local (No outbound sync)</option>
              <option value="metadata-shared">Metadata Shared (Run status & timestamps only)</option>
              <option value="content-shared">Content Shared (Includes session titles & summaries)</option>
              <option value="organization-knowledge">Organization Knowledge (Shared index)</option>
              <option value="public-marketplace">Public Marketplace (Public packages)</option>
            </select>
          </div>

          <div className="form-group">
            <label className="form-label">Data Retention (Days)</label>
            <input
              type="number"
              className="form-input"
              value={retentionDays ?? ""}
              onChange={(e) => setRetentionDays(e.target.value ? parseInt(e.target.value, 10) : undefined)}
              placeholder="e.g. 90 (leave empty for unlimited)"
              min={1}
              data-testid="retention-days-input"
            />
          </div>

          <div className="form-group">
            <label className="form-label">Data Residency (Region / Jurisdiction)</label>
            <input
              className="form-input"
              value={dataResidency}
              onChange={(e) => setDataResidency(e.target.value)}
              placeholder="e.g. eu-west-1, us-central1"
              data-testid="data-residency-input"
            />
          </div>
        </div>

        <div style={{ display: "flex", alignItems: "center", gap: "12px" }}>
          <button type="submit" className="btn btn-primary" disabled={isLoading} data-testid="save-settings-btn">
            <Save size={15} /> Save Policy Settings
          </button>
          {savedSuccess && (
            <span style={{ color: "var(--success)", fontSize: "13px", display: "flex", alignItems: "center", gap: "4px" }}>
              <Check size={15} /> Settings saved successfully
            </span>
          )}
        </div>
      </form>
    </div>
  );
}

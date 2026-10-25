import { useId } from "react";
import type { Mode } from "../generated/api";
import { dollars, parseDollars } from "../format";
import { words } from "../labels";
import { agentLabels as l } from "./labels";
import type { AgentChoices, AgentColor, AgentDraft, FileUse, ShellUse, WebUse } from "./types";

/** The four modes, in the order every mode control shows them (spec 3.3 §8.1). */
export const MODES: { mode: Mode; label: string }[] = [
  { mode: "justLook", label: words.JUST_LOOK },
  { mode: "suggest", label: words.SUGGEST },
  { mode: "askFirst", label: words.ASK_FIRST },
  { mode: "goAhead", label: words.GO_AHEAD },
];

const MODE_ORDER: Mode[] = ["justLook", "suggest", "askFirst", "goAhead"];

/** Whether `mode` asks less than `limit`: an agent's mode is its area's or stricter. */
export function looser(mode: Mode, limit: Mode): boolean {
  return MODE_ORDER.indexOf(mode) > MODE_ORDER.indexOf(limit);
}

/** A new draft in `area`, asking first and using nothing. */
export function emptyDraft(area: string, color: AgentColor = "green"): AgentDraft {
  return {
    displayName: "",
    color,
    description: "",
    instructions: "",
    area,
    mode: "askFirst",
    model: null,
    uses: { files: "none", folder: null, shell: "none", web: { access: "none" }, packs: [], drivers: [] },
    dailyLimitMicroUsd: null,
  };
}

function Choice<T extends string>({
  label,
  value,
  options,
  onChange,
  disabled,
}: {
  label: string;
  value: T;
  options: { value: T; label: string; disabled?: boolean }[];
  onChange: (value: T) => void;
  disabled?: boolean;
}) {
  const id = useId();
  return (
    <div className="agent-field">
      <span className="agent-label" id={id}>
        {label}
      </span>
      <div className="segmented agent-segmented" role="radiogroup" aria-labelledby={id}>
        {options.map((option) => (
          <button
            key={option.value}
            type="button"
            role="radio"
            aria-checked={option.value === value}
            className="segment"
            disabled={disabled || option.disabled}
            onClick={() => onChange(option.value)}
          >
            {option.label}
          </button>
        ))}
      </div>
    </div>
  );
}

function webAccess(web: WebUse): "none" | "listed" | "any" {
  return web.access;
}

/**
 * The definition's fields in one column, in the order of spec 3.3 §5.7 "The definition": the
 * form of **Set it up yourself** and of an agent's **Settings**. The area is fixed once an
 * agent exists (K-I15), the mode is its area's or stricter, and "Runs on" lists only the
 * models whose key the kernel has.
 */
export function AgentForm({
  draft,
  choices,
  onChange,
  areaFixed = false,
  disabled = false,
}: {
  draft: AgentDraft;
  choices: AgentChoices;
  onChange: (draft: AgentDraft) => void;
  areaFixed?: boolean;
  disabled?: boolean;
}) {
  const ids = { name: useId(), does: useId(), instructions: useId(), folder: useId(), sites: useId(), limit: useId(), model: useId(), area: useId() };
  const area = choices.areas.find((candidate) => candidate.id === draft.area);
  const set = (patch: Partial<AgentDraft>) => onChange({ ...draft, ...patch });
  const setUses = (patch: Partial<AgentDraft["uses"]>) => onChange({ ...draft, uses: { ...draft.uses, ...patch } });
  const toggle = (list: string[], item: string) => (list.includes(item) ? list.filter((x) => x !== item) : [...list, item]);
  const modelKey = draft.model ? `${draft.model.provider}/${draft.model.model}` : "";
  const hosts = draft.uses.web.access === "listed" ? draft.uses.web.hosts : [];

  return (
    <fieldset className="agent-form" disabled={disabled}>
      <div className="agent-field">
        <label className="agent-label" htmlFor={ids.name}>
          {l.name}
        </label>
        <input
          id={ids.name}
          className="agent-input"
          maxLength={40}
          value={draft.displayName}
          onChange={(event) => set({ displayName: event.target.value })}
        />
      </div>
      <div className="agent-field">
        <span className="agent-label">{l.colour}</span>
        <div className="agent-colours" role="radiogroup" aria-label={l.colour}>
          {choices.colors.map((choice) => (
            <button
              key={choice.color}
              type="button"
              role="radio"
              aria-checked={choice.color === draft.color}
              aria-label={choice.label}
              title={choice.label}
              className="agent-colour"
              style={{ background: choice.hex }}
              onClick={() => set({ color: choice.color })}
            />
          ))}
        </div>
      </div>
      <div className="agent-field">
        <label className="agent-label" htmlFor={ids.does}>
          {l.whatItDoes}
        </label>
        <input
          id={ids.does}
          className="agent-input"
          maxLength={200}
          value={draft.description}
          onChange={(event) => set({ description: event.target.value })}
        />
      </div>
      <div className="agent-field">
        <label className="agent-label" htmlFor={ids.instructions}>
          {l.instructions}
        </label>
        <textarea
          id={ids.instructions}
          className="agent-input agent-textarea"
          rows={5}
          value={draft.instructions}
          onChange={(event) => set({ instructions: event.target.value })}
        />
        <p className="agent-hint">{l.instructionsHint}</p>
      </div>
      <div className="agent-field">
        <label className="agent-label" htmlFor={ids.area}>
          {l.area}
        </label>
        <select
          id={ids.area}
          className="agent-input"
          value={draft.area}
          disabled={areaFixed || choices.areas.length < 2}
          onChange={(event) => {
            const next = choices.areas.find((candidate) => candidate.id === event.target.value);
            const mode = next && looser(draft.mode, next.mode.mode) ? next.mode.mode : draft.mode;
            onChange({ ...draft, area: event.target.value, mode, uses: { ...draft.uses, drivers: [] } });
          }}
        >
          {choices.areas.map((candidate) => (
            <option key={candidate.id} value={candidate.id}>
              {candidate.name}
            </option>
          ))}
        </select>
      </div>
      <Choice
        label={l.howMuchItAsksFirst}
        value={draft.mode}
        options={MODES.map(({ mode, label }) => ({
          value: mode,
          label,
          disabled: area ? looser(mode, area.mode.mode) : false,
        }))}
        onChange={(mode) => set({ mode })}
      />
      <div className="agent-field">
        <label className="agent-label" htmlFor={ids.model}>
          {l.runsOn}
        </label>
        <select
          id={ids.model}
          className="agent-input"
          value={modelKey}
          onChange={(event) => {
            const chosen = choices.models.find((model) => `${model.provider}/${model.model}` === event.target.value);
            set({ model: chosen ?? null });
          }}
        >
          <option value="">{l.primaryModel}</option>
          {choices.models.map((model) => (
            <option key={`${model.provider}/${model.model}`} value={`${model.provider}/${model.model}`}>
              {model.model}
            </option>
          ))}
        </select>
      </div>
      <p className="agent-subhead">{l.whatItCanUse}</p>
      <Choice<FileUse>
        label={l.files}
        value={draft.uses.files}
        options={(["none", "read", "readWrite"] as FileUse[]).map((value) => ({ value, label: l.files_[value] }))}
        onChange={(files) => setUses({ files, folder: files === "none" ? null : draft.uses.folder })}
      />
      {draft.uses.files !== "none" && (
        <div className="agent-field">
          <label className="agent-label" htmlFor={ids.folder}>
            {l.folder}
          </label>
          <input
            id={ids.folder}
            className="agent-input"
            value={draft.uses.folder ?? ""}
            onChange={(event) => setUses({ folder: event.target.value || null })}
          />
          <p className="agent-hint">{l.folderHint}</p>
        </div>
      )}
      <Choice<ShellUse>
        label={l.shell}
        value={draft.uses.shell}
        options={(["none", "sandboxed", "host"] as ShellUse[]).map((value) => ({ value, label: l.shell_[value] }))}
        onChange={(shell) => setUses({ shell })}
      />
      <Choice<"none" | "listed" | "any">
        label={l.web}
        value={webAccess(draft.uses.web)}
        options={(["none", "listed", "any"] as const).map((value) => ({ value, label: l.web_[value] }))}
        onChange={(access) =>
          setUses({ web: access === "listed" ? { access, hosts } : { access } })
        }
      />
      {draft.uses.web.access === "listed" && (
        <div className="agent-field">
          <label className="agent-label" htmlFor={ids.sites}>
            {l.sites}
          </label>
          <textarea
            id={ids.sites}
            className="agent-input agent-textarea"
            rows={3}
            value={hosts.join("\n")}
            onChange={(event) =>
              setUses({
                web: {
                  access: "listed",
                  hosts: event.target.value.split(/\s+/).filter((host) => host.length > 0),
                },
              })
            }
          />
          <p className="agent-hint">{l.sitesHint}</p>
        </div>
      )}
      {choices.packs.length > 0 && (
        <div className="agent-field">
          <span className="agent-label">{l.packs}</span>
          <div className="agent-checks">
            {choices.packs.map((pack) => (
              <label key={pack} className="agent-check">
                <input
                  type="checkbox"
                  checked={draft.uses.packs.includes(pack)}
                  onChange={() => setUses({ packs: toggle(draft.uses.packs, pack) })}
                />
                {pack}
              </label>
            ))}
          </div>
        </div>
      )}
      {area && area.drivers.length > 0 && (
        <div className="agent-field">
          <span className="agent-label">{l.drivers}</span>
          <div className="agent-checks">
            {area.drivers.map((driver) => (
              <label key={driver} className="agent-check">
                <input
                  type="checkbox"
                  checked={draft.uses.drivers.includes(driver)}
                  onChange={() => setUses({ drivers: toggle(draft.uses.drivers, driver) })}
                />
                {driver}
              </label>
            ))}
          </div>
        </div>
      )}
      <div className="agent-field">
        <label className="agent-label" htmlFor={ids.limit}>
          {l.dailyLimit}
        </label>
        <input
          id={ids.limit}
          className="agent-input agent-input-short"
          inputMode="decimal"
          defaultValue={draft.dailyLimitMicroUsd ? dollars(draft.dailyLimitMicroUsd).replace("$", "") : ""}
          onChange={(event) => set({ dailyLimitMicroUsd: parseDollars(event.target.value) ?? null })}
        />
        <p className="agent-hint">{l.dailyLimitHint}</p>
      </div>
    </fieldset>
  );
}

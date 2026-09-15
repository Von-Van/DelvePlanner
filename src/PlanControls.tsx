import { KeyboardEvent, useId, useRef } from "react";
import {
  Person,
  planColors,
  Plan,
  PlanColor,
  PlanStatus,
  Workstream,
} from "./api";
import { planColor, planColorValues, planStatusLabels } from "./planning";

export function PlanDot({ plan }: { plan: Pick<Plan, "color"> | undefined }) {
  return (
    <i
      className="plan-dot"
      style={{ background: planColor(plan) }}
      aria-hidden="true"
    />
  );
}

export function PlanChip({ plan }: { plan: Plan | undefined }) {
  if (!plan) return null;
  return (
    <span className="plan-chip" title={`Plan: ${plan.title}`}>
      <PlanDot plan={plan} />
      {plan.title}
    </span>
  );
}

export function StatusPill({ status }: { status: PlanStatus }) {
  return (
    <span className={`status-pill ${status}`}>{planStatusLabels[status]}</span>
  );
}

/** Lists active plans, plus the current plan when it has since been archived. */
export function PlanSelect({
  plans,
  value,
  onChange,
}: {
  plans: Plan[];
  value: string | null;
  onChange: (planId: string | null) => void;
}) {
  const options = plans.filter((plan) => !plan.archived || plan.id === value);
  return (
    <label>
      Plan
      <select
        value={value ?? ""}
        onChange={(input) => onChange(input.target.value || null)}
      >
        <option value="">No plan</option>
        {options.map((plan) => (
          <option key={plan.id} value={plan.id}>
            {plan.title}
            {plan.archived ? " (archived)" : ""}
          </option>
        ))}
      </select>
    </label>
  );
}

export function ColorSwatches({
  value,
  onChange,
}: {
  value: PlanColor | null;
  onChange: (color: PlanColor | null) => void;
}) {
  return (
    <div className="color-swatches" role="radiogroup" aria-label="Plan color">
      {planColors.map((color) => (
        <button
          type="button"
          key={color}
          role="radio"
          aria-checked={value === color}
          aria-label={color}
          title={color}
          className={value === color ? "selected" : ""}
          style={{ background: planColorValues[color] }}
          onClick={() => onChange(value === color ? null : color)}
        />
      ))}
    </div>
  );
}

/**
 * A native date input with an explicit way to return to "no date". The clear button sits outside
 * the <label> so clicking the label text focuses the input instead of activating the button.
 */
export function OptionalDate({
  label,
  value,
  onChange,
}: {
  label: string;
  value: string | null;
  onChange: (day: string | null) => void;
}) {
  const id = useId();
  return (
    <div className="editor-field">
      <span className="label-row">
        <label htmlFor={id}>{label}</label>
        {value && (
          <button
            type="button"
            className="clear-date"
            onClick={() => onChange(null)}
            aria-label={`Clear ${label.toLowerCase()}`}
          >
            Clear
          </button>
        )}
      </span>
      <input
        id={id}
        type="date"
        value={value ?? ""}
        onChange={(input) => onChange(input.target.value || null)}
      />
    </div>
  );
}

export function PersonSelect({
  people,
  value,
  onChange,
  label = "Owner",
}: {
  people: Person[];
  value: string | null;
  onChange: (personId: string | null) => void;
  label?: string;
}) {
  return (
    <label>
      {label}
      <select
        value={value ?? ""}
        onChange={(input) => onChange(input.target.value || null)}
      >
        <option value="">Unassigned</option>
        {people.map((person) => (
          <option key={person.id} value={person.id}>
            {person.displayName}
            {person.role ? ` — ${person.role}` : ""}
          </option>
        ))}
      </select>
    </label>
  );
}

export function WorkstreamSelect({
  workstreams,
  value,
  disabled,
  onChange,
}: {
  workstreams: Workstream[];
  value: string | null;
  disabled: boolean;
  onChange: (workstreamId: string | null) => void;
}) {
  return (
    <label>
      Workstream
      <select
        value={value ?? ""}
        disabled={disabled}
        onChange={(input) => onChange(input.target.value || null)}
      >
        <option value="">No workstream</option>
        {workstreams.map((workstream) => (
          <option key={workstream.id} value={workstream.id}>
            {workstream.name}
          </option>
        ))}
      </select>
    </label>
  );
}

export type TabItem<Key extends string> = {
  key: Key;
  label: string;
  count?: number | null;
};

/**
 * WAI-ARIA tabs: arrow keys, Home, and End move between tabs, and only the selected tab is in
 * the Tab order. The matching panel must use `tabPanelProps`.
 */
export function TabList<Key extends string>({
  id,
  label,
  tabs,
  value,
  onChange,
}: {
  id: string;
  label: string;
  tabs: TabItem<Key>[];
  value: Key;
  onChange: (key: Key) => void;
}) {
  const buttons = useRef(new Map<Key, HTMLButtonElement>());
  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    const index = tabs.findIndex((tab) => tab.key === value);
    const targets: Record<string, number> = {
      ArrowRight: (index + 1) % tabs.length,
      ArrowLeft: (index - 1 + tabs.length) % tabs.length,
      Home: 0,
      End: tabs.length - 1,
    };
    const target = targets[event.key];
    if (target === undefined) return;
    event.preventDefault();
    const key = tabs[target].key;
    onChange(key);
    buttons.current.get(key)?.focus();
  }
  return (
    <div
      className="plan-tabs"
      role="tablist"
      aria-label={label}
      onKeyDown={onKeyDown}
    >
      {tabs.map((tab) => {
        const selected = tab.key === value;
        return (
          <button
            key={tab.key}
            ref={(node) => {
              if (node) buttons.current.set(tab.key, node);
              else buttons.current.delete(tab.key);
            }}
            id={`${id}-tab-${tab.key}`}
            role="tab"
            aria-selected={selected}
            aria-controls={`${id}-panel`}
            tabIndex={selected ? 0 : -1}
            className={selected ? "active" : ""}
            onClick={() => onChange(tab.key)}
          >
            {tab.label}
            {tab.count != null && <span>{tab.count}</span>}
          </button>
        );
      })}
    </div>
  );
}

export function tabPanelProps(id: string, selected: string) {
  return {
    id: `${id}-panel`,
    role: "tabpanel",
    "aria-labelledby": `${id}-tab-${selected}`,
    tabIndex: 0,
  } as const;
}

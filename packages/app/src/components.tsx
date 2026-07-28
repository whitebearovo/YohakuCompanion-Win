import type { ReactNode } from "react";
import type { Tone } from "./labels";

export function Card(props: { title?: string; className?: string; children: ReactNode }) {
  return (
    <section className={props.className ? `card ${props.className}` : "card"}>
      {props.title ? <h2 className="card-title">{props.title}</h2> : null}
      {props.children}
    </section>
  );
}

export function Badge(props: { tone: Tone; children: ReactNode }) {
  return <span className={`badge badge-${props.tone}`}>{props.children}</span>;
}

export function Toggle(props: {
  checked: boolean;
  onChange: (value: boolean) => void;
  label: string;
  desc?: string;
  disabled?: boolean;
}) {
  return (
    <label className="toggle-row">
      <span className="toggle-text">
        <span className="toggle-label">{props.label}</span>
        {props.desc ? <span className="toggle-desc">{props.desc}</span> : null}
      </span>
      <input
        type="checkbox"
        className="switch"
        checked={props.checked}
        disabled={props.disabled ?? false}
        onChange={(e) => props.onChange(e.target.checked)}
      />
    </label>
  );
}

export function KvRow(props: { label: string; children: ReactNode }) {
  return (
    <div className="kv-row">
      <span className="kv-key">{props.label}</span>
      <span className="kv-val">{props.children}</span>
    </div>
  );
}

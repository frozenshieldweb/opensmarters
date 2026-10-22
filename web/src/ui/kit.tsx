// SPDX-License-Identifier: GPL-3.0-or-later
// Afterglow components (DESIGN.md §6): icons, buttons, fields, callouts, segmented controls,
// toggles, progress bars, empty states, skeletons. All provider text is rendered as text.

import type { ComponentChildren, JSX, Ref } from "preact";
import { useState } from "preact/hooks";
import { FILLED, ICONS, type FilledIconName, type IconName } from "./icons";

const cx = (...c: (string | false | null | undefined)[]) => c.filter(Boolean).join(" ");
export { cx };

// ---- icons

export function Icon(props: { name: IconName; fill?: boolean; class?: string; label?: string }) {
  const paths =
    props.fill && props.name in FILLED ? FILLED[props.name as FilledIconName] : ICONS[props.name];
  return (
    <svg
      class={cx("i", props.class)}
      viewBox="0 0 256 256"
      aria-hidden={props.label ? undefined : "true"}
      role={props.label ? "img" : undefined}
      aria-label={props.label}
      focusable="false"
    >
      {paths.map((d) => (
        <path d={d} key={d.slice(0, 24)} />
      ))}
    </svg>
  );
}

/** The brand symbol "Open corners" (DESIGN.md §2), champagne flowing into rose. */
export function BrandSymbol(props: { class?: string }) {
  return (
    <svg class={cx("brand-symbol", props.class)} viewBox="102 184 820 656" aria-hidden="true" focusable="false">
      <defs>
        <linearGradient id="os-sym" x1="102.4" y1="184.3" x2="921.6" y2="839.7" gradientUnits="userSpaceOnUse">
          <stop offset="0" stop-color="#E6BE91" />
          <stop offset="0.36" stop-color="#E6BE91" />
          <stop offset="1" stop-color="#C9868A" />
        </linearGradient>
      </defs>
      <g fill="url(#os-sym)">
        <path d="M 707.825 184.32 A 16.319 16.319 0 0 1 724.143 200.639 L 724.143 282.232 A 16.319 16.319 0 0 1 707.825 298.551 L 314.543 298.551 A 97.912 97.912 0 0 0 216.631 396.463 L 216.631 602.732 A 16.319 16.319 0 0 1 200.312 619.051 L 118.719 619.051 A 16.319 16.319 0 0 1 102.4 602.732 L 102.4 396.463 A 212.143 212.143 0 0 1 314.543 184.32 Z" />
        <path d="M 316.175 839.68 A 16.319 16.319 0 0 1 299.857 823.361 L 299.857 741.768 A 16.319 16.319 0 0 1 316.175 725.449 L 709.457 725.449 A 97.912 97.912 0 0 0 807.369 627.537 L 807.369 421.268 A 16.319 16.319 0 0 1 823.688 404.949 L 905.281 404.949 A 16.319 16.319 0 0 1 921.6 421.268 L 921.6 627.537 A 212.143 212.143 0 0 1 709.457 839.68 Z" />
      </g>
    </svg>
  );
}

/** The live wordmark: lowercase, Fraunces, one colour (DESIGN.md §2, LAUNCH_PROMPT §0). */
export function Lockup(props: { class?: string }) {
  return (
    <span class={cx("lockup", props.class)}>
      <BrandSymbol />
      <span class="wordmark">opensmarters</span>
    </span>
  );
}

// ---- buttons (§6.5)

export type ButtonVariant = "primary" | "secondary" | "ghost" | "danger";

interface ButtonProps extends Omit<JSX.ButtonHTMLAttributes<HTMLButtonElement>, "icon" | "size" | "type"> {
  variant?: ButtonVariant;
  size?: "md" | "lg" | "sm";
  icon?: IconName;
  iconFill?: boolean;
  iconRight?: IconName;
  /** Focus key (focus restoration). */
  fk?: string;
  autofocus?: boolean;
  busy?: boolean;
  block?: boolean;
  buttonRef?: Ref<HTMLButtonElement>;
  type?: "button" | "submit";
}

export function Button({
  variant = "secondary",
  size = "md",
  icon,
  iconFill,
  iconRight,
  fk,
  autofocus,
  busy,
  block,
  buttonRef,
  children,
  class: klass,
  type = "button",
  ...rest
}: ButtonProps) {
  return (
    <button
      {...rest}
      ref={buttonRef}
      type={type}
      class={cx("btn", `btn-${variant}`, size !== "md" && `btn-${size}`, block && "btn-block", busy && "is-busy", klass as string)}
      data-fk={fk}
      data-autofocus={autofocus ? "" : undefined}
      aria-busy={busy ? "true" : undefined}
    >
      {busy ? <Spinner small /> : icon ? <Icon name={icon} fill={iconFill} /> : null}
      {children !== undefined && <span class="btn-label">{children}</span>}
      {iconRight && <Icon name={iconRight} />}
    </button>
  );
}

interface IconButtonProps extends Omit<JSX.ButtonHTMLAttributes<HTMLButtonElement>, "icon" | "size"> {
  icon: IconName;
  label: string;
  fill?: boolean;
  fk?: string;
  size?: "md" | "lg" | "osd";
  active?: boolean;
  autofocus?: boolean;
}

/** A circular icon button (44 px; OSD 48 px). The label is for screen readers and the tooltip. */
export function IconButton({ icon, label, fill, fk, size = "md", active, autofocus, class: klass, ...rest }: IconButtonProps) {
  return (
    <button
      type="button"
      {...rest}
      class={cx("icon-btn", size !== "md" && `icon-btn-${size}`, active && "is-active", klass as string)}
      aria-label={label}
      title={label}
      data-fk={fk}
      data-autofocus={autofocus ? "" : undefined}
    >
      <Icon name={icon} fill={fill} />
    </button>
  );
}

// ---- fields (§6.7)

interface FieldProps {
  label: string;
  value: string;
  onInput: (v: string) => void;
  type?: "text" | "password" | "url" | "search";
  placeholder?: string;
  error?: string | null;
  hint?: string;
  autocomplete?: string;
  autofocus?: boolean;
  name?: string;
  inputMode?: "text" | "url" | "numeric";
  maxLength?: number;
  fk?: string;
  spellcheck?: boolean;
}

let fieldSeq = 0;

export function Field(p: FieldProps) {
  const [id] = useState(() => `f${++fieldSeq}`);
  const [reveal, setReveal] = useState(false);
  const isPw = p.type === "password";
  return (
    <div class={cx("field", p.error && "has-error")}>
      <label class="field-label t-caption" for={id}>
        {p.label}
      </label>
      <div class="field-box">
        <input
          id={id}
          name={p.name}
          class="field-input"
          type={isPw && !reveal ? "password" : p.type === "url" ? "url" : "text"}
          value={p.value}
          placeholder={p.placeholder}
          autocomplete={p.autocomplete ?? "off"}
          autocapitalize="off"
          spellcheck={p.spellcheck ?? false}
          inputMode={p.inputMode}
          maxLength={p.maxLength ?? 1024}
          aria-invalid={p.error ? "true" : undefined}
          aria-describedby={p.error ? `${id}-err` : p.hint ? `${id}-hint` : undefined}
          data-autofocus={p.autofocus ? "" : undefined}
          data-fk={p.fk}
          onInput={(e) => p.onInput((e.currentTarget as HTMLInputElement).value)}
        />
        {isPw && (
          <button
            type="button"
            class="field-reveal"
            aria-label={reveal ? "Hide password" : "Show password"}
            title={reveal ? "Hide password" : "Show password"}
            aria-pressed={reveal ? "true" : "false"}
            onClick={() => setReveal((r) => !r)}
          >
            <Icon name={reveal ? "eye-slash" : "eye"} />
          </button>
        )}
      </div>
      {p.error ? (
        <p class="field-error t-caption" id={`${id}-err`} role="alert">
          <Icon name="warning" />
          <span>{p.error}</span>
        </p>
      ) : p.hint ? (
        <p class="field-hint t-caption" id={`${id}-hint`}>
          {p.hint}
        </p>
      ) : null}
    </div>
  );
}

// ---- callouts (§6.17)

export function Callout(props: {
  kind: "warning" | "danger" | "info" | "success";
  lead?: string;
  children?: ComponentChildren;
  class?: string;
}) {
  const icon: IconName =
    props.kind === "warning" ? "warning" : props.kind === "danger" ? "warning-circle" : props.kind === "success" ? "check-circle" : "info";
  return (
    <div class={cx("callout", `callout-${props.kind}`, props.class)} role={props.kind === "danger" ? "alert" : "note"}>
      <Icon name={icon} class="callout-icon" />
      <p>
        {props.lead && <strong>{props.lead} </strong>}
        {props.children}
      </p>
    </div>
  );
}

// ---- segmented control (§6.6)

export function Segmented<T extends string>(props: {
  options: { value: T; label: string }[];
  value: T;
  onChange: (v: T) => void;
  label: string;
  fkPrefix?: string;
}) {
  const idx = Math.max(0, props.options.findIndex((o) => o.value === props.value));
  return (
    <div class="segmented" role="radiogroup" aria-label={props.label} data-scope={`seg-${props.label}`}>
      <span
        class="segmented-pill"
        aria-hidden="true"
        style={{ width: `calc((100% - 8px) / ${props.options.length})`, transform: `translateX(${idx * 100}%)` }}
      />
      {props.options.map((o) => (
        <button
          type="button"
          key={o.value}
          role="radio"
          aria-checked={o.value === props.value ? "true" : "false"}
          class={cx("segmented-opt", o.value === props.value && "is-selected")}
          data-fk={props.fkPrefix ? `${props.fkPrefix}${o.value}` : undefined}
          onClick={() => props.onChange(o.value)}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

// ---- toggle (§8.6)

export function Toggle(props: { on: boolean; onChange: (on: boolean) => void; label: string; fk?: string }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={props.on ? "true" : "false"}
      aria-label={props.label}
      class={cx("toggle", props.on && "is-on")}
      data-fk={props.fk}
      onClick={() => props.onChange(!props.on)}
    >
      <span class="toggle-knob" />
    </button>
  );
}

// ---- progress

export function Bar(props: { value: number; class?: string; label?: string }) {
  const pct = `${Math.round(Math.min(1, Math.max(0, props.value)) * 1000) / 10}%`;
  return (
    <span
      class={cx("bar", props.class)}
      role={props.label ? "progressbar" : undefined}
      aria-label={props.label}
      aria-valuenow={props.label ? Math.round(props.value * 100) : undefined}
      aria-valuemin={props.label ? 0 : undefined}
      aria-valuemax={props.label ? 100 : undefined}
    >
      <span class="bar-fill" style={{ width: pct }} />
    </span>
  );
}

export function Spinner(props: { small?: boolean; label?: string }) {
  return (
    <span class={cx("spinner", props.small && "spinner-sm")} role={props.label ? "status" : undefined} aria-label={props.label}>
      <svg viewBox="0 0 48 48" aria-hidden="true" focusable="false">
        <circle cx="24" cy="24" r="20" />
      </svg>
    </span>
  );
}

// ---- empty and error states (§6.13)

export type Illustration = "empty-favourites" | "empty-search" | "error-offline" | "empty-guide";

export function Empty(props: {
  art?: Illustration;
  title: string;
  body?: ComponentChildren;
  action?: ComponentChildren;
  class?: string;
  compact?: boolean;
}) {
  return (
    <div class={cx("empty", props.compact && "empty-compact", props.class)}>
      {props.art && <img class="empty-art" src={`/img/${props.art}.png`} alt="" width="200" height="200" />}
      <h2 class="empty-title">{props.title}</h2>
      {props.body && <p class="empty-body c-2">{props.body}</p>}
      {props.action && <div class="empty-action">{props.action}</div>}
    </div>
  );
}

/** An error with a Retry button, in plain words. */
export function ErrorState(props: { message: string; onRetry?: () => void; compact?: boolean; title?: string }) {
  return (
    <Empty
      art={props.compact ? undefined : "error-offline"}
      compact={props.compact}
      title={props.title ?? "Something went wrong"}
      body={props.message}
      action={
        props.onRetry && (
          <Button variant="primary" icon="arrows-clockwise" onClick={props.onRetry} autofocus>
            Try again
          </Button>
        )
      }
    />
  );
}

// ---- skeletons (§6.16)

export function Skeleton(props: { class?: string }) {
  return <span class={cx("skeleton", props.class)} aria-hidden="true" />;
}

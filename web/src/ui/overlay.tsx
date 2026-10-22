// SPDX-License-Identifier: GPL-3.0-or-later
// Layers above the shell: dialogs (§6.11), anchored menus, toasts (§6.12). An open layer is a
// `[data-modal]` element; the shell underneath becomes `inert`, so focus and arrows stay in it.
// Esc / Backspace close the top layer; the focus returns to the element that opened it.

import type { ComponentChildren, VNode } from "preact";
import { computed, signal } from "@preact/signals";
import { useEffect, useLayoutEffect, useRef } from "preact/hooks";
import { focusEl, focusFallback, pushBack } from "../focus/dom";
import { toasts } from "../state/misc";
import { Button, cx, Icon } from "./kit";

/** Closes a layer; `restoreFocus: false` when the action itself moves the focus (navigation). */
export type CloseLayer = (restoreFocus?: boolean) => void;

interface Layer {
  id: number;
  render: (close: CloseLayer) => VNode;
  close: CloseLayer;
}

const layers = signal<Layer[]>([]);
let layerId = 1;

/** True while a dialog or menu is open (the shell is inert). */
export const layerOpen = computed(() => layers.value.length > 0);

/** Opens a layer; resolves when it closes. */
export function openLayer(render: (close: CloseLayer) => VNode): Promise<void> {
  return new Promise((resolve) => {
    const opener = document.activeElement as HTMLElement | null;
    const id = layerId++;
    const close: CloseLayer = (restoreFocus = true) => {
      if (!layers.value.some((l) => l.id === id)) return;
      layers.value = layers.value.filter((l) => l.id !== id);
      resolve();
      if (!restoreFocus) return;
      // Back to the element that opened it (if it's still there).
      requestAnimationFrame(() => {
        if (opener && opener.isConnected && !opener.closest("[inert]")) focusEl(opener, { scroll: false });
        else focusFallback();
      });
    };
    layers.value = [...layers.value, { id, render, close }];
  });
}

/** Asks a question; resolves true for the confirm action. Initial focus is on the safe action. */
export function confirmDialog(opts: {
  title: string;
  body: ComponentChildren;
  confirm: string;
  cancel?: string;
  danger?: boolean;
}): Promise<boolean> {
  let result = false;
  return openLayer((close) => (
    <Dialog title={opts.title} onClose={close}>
      <div class="dialog-body c-2">{opts.body}</div>
      <div class="dialog-actions">
        <Button variant="secondary" onClick={() => close()} autofocus>
          {opts.cancel ?? "Cancel"}
        </Button>
        <Button
          variant={opts.danger ? "danger" : "primary"}
          onClick={() => {
            result = true;
            close();
          }}
        >
          {opts.confirm}
        </Button>
      </div>
    </Dialog>
  )).then(() => result);
}

function useLayerFocus(ref: { current: HTMLElement | null }, onClose: () => void) {
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const auto = el.querySelector<HTMLElement>("[data-autofocus]");
    if (auto) focusEl(auto, { scroll: false });
    else focusFallback(el);
  }, []);
  useEffect(() => pushBack(() => (onClose(), true)), [onClose]);
}

/** A centred dialog: title, body, actions (primary rightmost). */
export function Dialog(props: { title: string; onClose: () => void; children: ComponentChildren; wide?: boolean }) {
  const ref = useRef<HTMLDivElement>(null);
  useLayerFocus(ref, props.onClose);
  return (
    <div class="layer layer-dialog" data-modal="" onPointerDown={(e) => e.target === e.currentTarget && props.onClose()}>
      <div ref={ref} class={cx("dialog", props.wide && "dialog-wide")} role="dialog" aria-modal="true" aria-label={props.title} data-scope="dialog" data-group="dialog">
        <h2 class="dialog-title t-title">{props.title}</h2>
        {props.children}
      </div>
    </div>
  );
}

export interface MenuItem {
  label: string;
  hint?: string;
  icon?: Parameters<typeof Icon>[0]["name"];
  selected?: boolean;
  onSelect: () => void;
}

/** A menu anchored to the button that opened it (sort order, account switcher). */
export function openMenu(anchor: HTMLElement, title: string, items: MenuItem[], placement: "below" | "above" = "below"): Promise<void> {
  const r = anchor.getBoundingClientRect();
  return openLayer((close) => <Menu title={title} items={items} rect={r} placement={placement} onClose={close} />);
}

function Menu(props: { title: string; items: MenuItem[]; rect: DOMRect; placement: "below" | "above"; onClose: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  useLayerFocus(ref, props.onClose);
  const vw = window.innerWidth;
  const vh = window.innerHeight;
  const width = Math.min(320, vw - 32);
  const left = Math.min(Math.max(16, props.rect.left), vw - width - 16);
  const pos =
    props.placement === "below"
      ? { top: `${Math.min(props.rect.bottom + 8, vh - 120)}px`, left: `${left}px`, width: `${width}px` }
      : { bottom: `${Math.max(16, vh - props.rect.top + 8)}px`, left: `${left}px`, width: `${width}px` };
  const sel = props.items.findIndex((i) => i.selected);
  return (
    <div class="layer layer-menu" data-modal="" onPointerDown={(e) => e.target === e.currentTarget && props.onClose()}>
      <div ref={ref} class="menu" role="menu" aria-label={props.title} style={pos} data-scope="menu" data-group="menu">
        <p class="menu-title t-overline c-3">{props.title}</p>
        {props.items.map((it, i) => (
          <button
            type="button"
            role="menuitemradio"
            aria-checked={it.selected ? "true" : "false"}
            key={it.label}
            class={cx("menu-item", it.selected && "is-selected")}
            data-autofocus={i === (sel >= 0 ? sel : 0) ? "" : undefined}
            onClick={() => {
              props.onClose();
              it.onSelect();
            }}
          >
            {it.icon && <Icon name={it.icon} />}
            <span class="menu-label ellipsis">{it.label}</span>
            {it.hint && <span class="menu-hint t-caption c-3">{it.hint}</span>}
            {it.selected && <Icon name="check" class="menu-check" />}
          </button>
        ))}
      </div>
    </div>
  );
}

/** Renders the open layers (in the app root, above the shell). */
export function Layers() {
  return (
    <>
      {layers.value.map((l) => (
        <LayerHost key={l.id} layer={l} />
      ))}
    </>
  );
}

function LayerHost(props: { layer: Layer }) {
  return props.layer.render(props.layer.close);
}

/** Toasts, bottom centre, never focusable (§6.12). */
export function Toasts() {
  return (
    <div class="toasts" role="status" aria-live="polite">
      {toasts.value.map((t) => (
        <div class={cx("toast", `toast-${t.kind}`)} key={t.id}>
          <Icon
            name={t.kind === "error" ? "warning-circle" : t.kind === "warning" ? "warning" : t.kind === "success" ? "check-circle" : "info"}
            class="toast-icon"
          />
          <span>{t.text}</span>
        </div>
      ))}
    </div>
  );
}

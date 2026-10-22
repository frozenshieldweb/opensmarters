// SPDX-License-Identifier: GPL-3.0-or-later
// Pieces shared by several screens: the chip row of categories (§6.6) and the screen header.

import type { ComponentChildren } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { cx } from "../ui/kit";

export interface Chip {
  id: string;
  label: string;
}

/** A horizontally scrolling row of category chips; edges with hidden chips fade out. */
export function ChipRow(props: { items: Chip[]; selected: string; onSelect: (id: string) => void; scope: string; label?: string; trailing?: ComponentChildren }) {
  const ref = useRef<HTMLDivElement>(null);
  const [edges, setEdges] = useState({ start: false, end: false });
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const read = () =>
      setEdges({ start: el.scrollLeft > 2, end: el.scrollLeft + el.clientWidth < el.scrollWidth - 2 });
    read();
    el.addEventListener("scroll", read, { passive: true });
    const ro = new ResizeObserver(read);
    ro.observe(el);
    return () => {
      el.removeEventListener("scroll", read);
      ro.disconnect();
    };
  }, [props.items.length]);
  return (
    <div class="chiprow-wrap">
      <div
        ref={ref}
        class={cx("chiprow", edges.start && "fade-start", edges.end && "fade-end")}
        role="toolbar"
        aria-label={props.label ?? "Categories"}
        data-scope={props.scope}
      >
        {props.items.map((c) => (
          <button
            type="button"
            key={c.id}
            class={cx("chip", c.id === props.selected && "is-selected")}
            aria-pressed={c.id === props.selected ? "true" : "false"}
            data-fk={`chip:${c.id}`}
            onClick={() => props.onSelect(c.id)}
          >
            {c.label}
          </button>
        ))}
      </div>
      {props.trailing}
    </div>
  );
}

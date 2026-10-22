// SPDX-License-Identifier: GPL-3.0-or-later
// The one <video> element of the app, fixed above the shell. In Live TV it sits exactly over
// the preview's placeholder; in the player it fills the window. Keeping a single element means
// going fullscreen (and back) never restarts the stream.

import { signal } from "@preact/signals";
import { useEffect, useLayoutEffect, useRef, useState } from "preact/hooks";
import { current } from "../state/router";
import { attachVideo, item } from "./playback";

/** The placeholder the preview video is drawn over (set by Live TV's preview). */
export const previewTarget = signal<HTMLElement | null>(null);

interface Box {
  top: number;
  left: number;
  width: number;
  height: number;
}

export function VideoLayer() {
  const ref = useRef<HTMLVideoElement>(null);
  const [box, setBox] = useState<Box | null>(null);
  const full = current.value.route.name === "watch";
  const target = previewTarget.value;
  const active = !!item.value;

  useLayoutEffect(() => {
    if (ref.current) attachVideo(ref.current);
  }, []);

  useEffect(() => {
    if (full || !target) {
      setBox(null);
      return;
    }
    let frame = 0;
    const read = () => {
      frame = 0;
      const r = target.getBoundingClientRect();
      setBox((b) =>
        b && b.top === r.top && b.left === r.left && b.width === r.width && b.height === r.height
          ? b
          : { top: r.top, left: r.left, width: r.width, height: r.height },
      );
    };
    const schedule = () => {
      if (!frame) frame = requestAnimationFrame(read);
    };
    read();
    const ro = new ResizeObserver(schedule);
    ro.observe(target);
    ro.observe(document.body);
    window.addEventListener("resize", schedule);
    // Layout can shift without a resize (fonts, chips): re-measure for a moment.
    const t = setInterval(schedule, 500);
    return () => {
      ro.disconnect();
      window.removeEventListener("resize", schedule);
      clearInterval(t);
      if (frame) cancelAnimationFrame(frame);
    };
  }, [full, target]);

  const visible = active && (full || !!box);
  const style = full || !box ? {} : { top: `${box.top}px`, left: `${box.left}px`, width: `${box.width}px`, height: `${box.height}px` };
  return (
    <div class={`video-layer ${full ? "is-full" : "is-preview"} ${visible ? "" : "is-hidden"}`} style={style} aria-hidden={full ? undefined : "true"}>
      <video ref={ref} class="video" playsInline preload="none" disablePictureInPicture />
    </div>
  );
}

// SPDX-License-Identifier: GPL-3.0-or-later
// The Phosphor icons the UI uses (MIT; DESIGN.md §4.8), imported as path data from
// @phosphor-icons/core at build time (vite.config.ts, plugin `opensmarters:phosphor`).
// Regular at rest; Fill for selected / on states. Add an import here to use a new icon.

import house from "~icons/regular/house";
import televisionSimple from "~icons/regular/television-simple";
import rows from "~icons/regular/rows";
import filmStrip from "~icons/regular/film-strip";
import monitorPlay from "~icons/regular/monitor-play";
import magnifyingGlass from "~icons/regular/magnifying-glass";
import heart from "~icons/regular/heart";
import gearSix from "~icons/regular/gear-six";
import userCircle from "~icons/regular/user-circle";
import caretLeft from "~icons/regular/caret-left";
import caretRight from "~icons/regular/caret-right";
import caretDown from "~icons/regular/caret-down";
import caretUp from "~icons/regular/caret-up";
import play from "~icons/regular/play";
import pause from "~icons/regular/pause";
import arrowCounterClockwise from "~icons/regular/arrow-counter-clockwise";
import arrowClockwise from "~icons/regular/arrow-clockwise";
import subtitles from "~icons/regular/subtitles";
import speakerHigh from "~icons/regular/speaker-high";
import speakerSlash from "~icons/regular/speaker-slash";
import arrowsOut from "~icons/regular/arrows-out";
import arrowsIn from "~icons/regular/arrows-in";
import warning from "~icons/regular/warning";
import warningCircle from "~icons/regular/warning-circle";
import wifiSlash from "~icons/regular/wifi-slash";
import broadcast from "~icons/regular/broadcast";
import info from "~icons/regular/info";
import check from "~icons/regular/check";
import checkCircle from "~icons/regular/check-circle";
import x from "~icons/regular/x";
import eye from "~icons/regular/eye";
import eyeSlash from "~icons/regular/eye-slash";
import plus from "~icons/regular/plus";
import signOut from "~icons/regular/sign-out";
import list from "~icons/regular/list";
import arrowsClockwise from "~icons/regular/arrows-clockwise";
import trash from "~icons/regular/trash";
import pencilSimple from "~icons/regular/pencil-simple";
import link from "~icons/regular/link";
import copy from "~icons/regular/copy";
import lockSimple from "~icons/regular/lock-simple";
import clockCounterClockwise from "~icons/regular/clock-counter-clockwise";
import star from "~icons/regular/star";
import userPlus from "~icons/regular/user-plus";
import users from "~icons/regular/users";
import prohibit from "~icons/regular/prohibit";
import shieldCheck from "~icons/regular/shield-check";
import key from "~icons/regular/key";
import sortAscending from "~icons/regular/sort-ascending";
import skipForward from "~icons/regular/skip-forward";
import arrowUpRight from "~icons/regular/arrow-up-right";
import code from "~icons/regular/code";
import clock from "~icons/regular/clock";
import circleNotch from "~icons/regular/circle-notch";
import globeSimple from "~icons/regular/globe-simple";
import houseFill from "~icons/fill/house";
import televisionSimpleFill from "~icons/fill/television-simple";
import rowsFill from "~icons/fill/rows";
import filmStripFill from "~icons/fill/film-strip";
import monitorPlayFill from "~icons/fill/monitor-play";
import magnifyingGlassFill from "~icons/fill/magnifying-glass";
import heartFill from "~icons/fill/heart";
import gearSixFill from "~icons/fill/gear-six";
import playFill from "~icons/fill/play";
import pauseFill from "~icons/fill/pause";
import starFill from "~icons/fill/star";
import speakerHighFill from "~icons/fill/speaker-high";

export const ICONS = {
  "house": house,
  "television-simple": televisionSimple,
  "rows": rows,
  "film-strip": filmStrip,
  "monitor-play": monitorPlay,
  "magnifying-glass": magnifyingGlass,
  "heart": heart,
  "gear-six": gearSix,
  "user-circle": userCircle,
  "caret-left": caretLeft,
  "caret-right": caretRight,
  "caret-down": caretDown,
  "caret-up": caretUp,
  "play": play,
  "pause": pause,
  "arrow-counter-clockwise": arrowCounterClockwise,
  "arrow-clockwise": arrowClockwise,
  "subtitles": subtitles,
  "speaker-high": speakerHigh,
  "speaker-slash": speakerSlash,
  "arrows-out": arrowsOut,
  "arrows-in": arrowsIn,
  "warning": warning,
  "warning-circle": warningCircle,
  "wifi-slash": wifiSlash,
  "broadcast": broadcast,
  "info": info,
  "check": check,
  "check-circle": checkCircle,
  "x": x,
  "eye": eye,
  "eye-slash": eyeSlash,
  "plus": plus,
  "sign-out": signOut,
  "list": list,
  "arrows-clockwise": arrowsClockwise,
  "trash": trash,
  "pencil-simple": pencilSimple,
  "link": link,
  "copy": copy,
  "lock-simple": lockSimple,
  "clock-counter-clockwise": clockCounterClockwise,
  "star": star,
  "user-plus": userPlus,
  "users": users,
  "prohibit": prohibit,
  "shield-check": shieldCheck,
  "key": key,
  "sort-ascending": sortAscending,
  "skip-forward": skipForward,
  "arrow-up-right": arrowUpRight,
  "code": code,
  "clock": clock,
  "circle-notch": circleNotch,
  "globe-simple": globeSimple,
} as const;

export const FILLED = {
  "house": houseFill,
  "television-simple": televisionSimpleFill,
  "rows": rowsFill,
  "film-strip": filmStripFill,
  "monitor-play": monitorPlayFill,
  "magnifying-glass": magnifyingGlassFill,
  "heart": heartFill,
  "gear-six": gearSixFill,
  "play": playFill,
  "pause": pauseFill,
  "star": starFill,
  "speaker-high": speakerHighFill,
} as const;

export type IconName = keyof typeof ICONS;
export type FilledIconName = keyof typeof FILLED;

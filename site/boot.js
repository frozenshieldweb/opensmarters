// SPDX-License-Identifier: GPL-3.0-or-later
// Runs before first paint (a classic script in <head>): marks that JavaScript runs and which
// download variant fits this device, so CSS shows the right one without a layout shift.
(function () {
  var d = document.documentElement;
  var nav = window.navigator;
  var ua = nav.userAgent || "";
  var data = nav.userAgentData;
  var platform = (data && data.platform) || nav.platform || "";
  var touchMac = /Mac/i.test(platform) && nav.maxTouchPoints > 1; // iPadOS asks for the desktop site
  var mobile = (data && data.mobile) || touchMac || /Android|iPhone|iPad|iPod|Mobile/i.test(ua);
  var kind = mobile ? "mobile" : /Win/i.test(platform) || /Windows NT/.test(ua) ? "win" : "other";
  d.setAttribute("data-js", "");
  d.setAttribute("data-platform", kind);
})();

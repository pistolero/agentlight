#pragma once
#include "app_state.h"
#include "wifi_link.h"

// Startup / connection screen: what the display is doing and how to fix it.
namespace BootScreen {
enum Icon : uint8_t { Done, Busy, Waiting, Off };

inline void row(int x, int y, int w, Icon icon, const String &title, const String &detail, uint16_t accent) {
  using namespace Theme;
  int cx = x + 40, cy = y + 32;
  if (icon == Done) canvas.check(cx, cy, 12, WORKING);
  else if (icon == Busy) { canvas.arc(cx, cy, 12, 2.5f, 0, 360, accent); canvas.fillCircle(cx, cy, 4, accent); }
  else if (icon == Waiting) canvas.fillCircle(cx, cy, 6, accent);
  else canvas.arc(cx, cy, 12, 2.5f, 0, 360, TEXT3);
  canvas.text(x + 72, y + 9, title, font_s20, icon == Off ? TEXT2 : TEXT);
  canvas.text(x + 72, y + 36, canvas.fit(detail, font_s16, w - 96), font_s16, TEXT2);
}

inline uint32_t key() {
  FrameKey k; k.add(2u);
  k.add(uint32_t(WifiLink::linkState)); k.add(uint32_t(everConnected)); k.add(WifiLink::ssid());
  return k.h;
}

inline void draw() {
  using namespace Theme;
  canvas.fillScreen(BG);
  canvas.logo(512, 96, 20);
  canvas.textCenter(512, 128, "agentlight", font_s56, TEXT);
  canvas.textCenter(512, 200, "Live status for your coding agents", font_s20, TEXT2);

  constexpr int X = 212, W = 600, Y = 252, ROW = 64;
  canvas.fillRoundRect(X, Y, W, ROW * 3, RADIUS, SURFACE);
  canvas.fillRect(X + 72, Y + ROW, W - 96, 1, DIVIDER);
  canvas.fillRect(X + 72, Y + ROW * 2, W - 96, 1, DIVIDER);

  row(X, Y, W, Done, "Display ready", String("Firmware ") + FW_VERSION, WORKING);

  uint8_t link = WifiLink::linkState;
  String ssid = WifiLink::ssid();
  if (link == WifiLink::NoConfig) row(X, Y + ROW, W, Off, "Wi-Fi not set up", "Optional - set up once over USB with agentlight --wifi-setup", TEXT3);
  else if (link == WifiLink::Joining) row(X, Y + ROW, W, Busy, "Joining Wi-Fi", ssid, DONE);
  else row(X, Y + ROW, W, Done, "Wi-Fi connected", ssid, WORKING);

  if (everConnected) row(X, Y + ROW * 2, W, Busy, "Connection lost", "Reconnecting to the agentlight helper...", WAITING);
  else if (link == WifiLink::Authorized) row(X, Y + ROW * 2, W, Busy, "Helper connected", "Waiting for the first status update...", WORKING);
  else if (link == WifiLink::Connecting) row(X, Y + ROW * 2, W, Busy, "Connecting to helper", "Verifying the approved helper...", DONE);
  else row(X, Y + ROW * 2, W, Busy, "Looking for the helper", link == WifiLink::NoConfig ? "Waiting for USB" : "Waiting for USB or a helper on this network", DONE);

  const char *hint = link == WifiLink::NoConfig
    ? "Connect USB and run  agentlight  on your computer."
    : "Run  agentlight  with USB, or  agentlight --wifi  on the same network.";
  canvas.textCenter(512, Y + ROW * 3 + 28, hint, font_s16, TEXT3);

  canvas.text(MARGIN + 8, HEIGHT - 36, "Display " + WifiLink::deviceId(), font_s16, TEXT3);
  canvas.textRight(WIDTH - MARGIN - 8, HEIGHT - 36, String("v") + FW_VERSION, font_s16, TEXT3);
}
}

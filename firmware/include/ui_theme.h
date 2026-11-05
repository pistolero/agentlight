#pragma once
#include <Arduino.h>
#include "lato_font.h"

// Design tokens for every screen. Colors are RGB565, written as hex RGB.
constexpr uint16_t rgb(uint32_t hex) {
  return uint16_t(((hex >> 8) & 0xF800) | ((hex >> 5) & 0x07E0) | ((hex >> 3) & 0x001F));
}
// Blend b over a with alpha 0..255 (compile-time friendly).
constexpr uint16_t mix(uint16_t a, uint16_t b, uint8_t alpha) {
  return uint16_t(
    ((((a >> 11) & 31) * (255 - alpha) + ((b >> 11) & 31) * alpha) / 255) << 11 |
    ((((a >> 5) & 63) * (255 - alpha) + ((b >> 5) & 63) * alpha) / 255) << 5 |
    (((a & 31) * (255 - alpha) + (b & 31) * alpha) / 255));
}

namespace Theme {
constexpr uint16_t BG = rgb(0x0E1116), SURFACE = rgb(0x171C23), RAISED = rgb(0x212833), DIVIDER = rgb(0x2A323D);
constexpr uint16_t TEXT = rgb(0xE8EDF2), TEXT2 = rgb(0xA3AFBC), TEXT3 = rgb(0x66727F);
constexpr uint16_t WORKING = rgb(0x3FB950), WAITING = rgb(0xE3A008), ERROR_ = rgb(0xF85149), IDLE = rgb(0x8B949E), DONE = rgb(0x58A6FF);
constexpr uint16_t USER = rgb(0x58A6FF), AGENT = rgb(0xBC8CFF);
constexpr int MARGIN = 16, GAP = 12, RADIUS = 14;
}

struct StatusStyle { uint16_t color, tint, pill; const char *label; };
// Card tint: the status color at ~18% over the surface; pill a bit stronger.
inline StatusStyle statusStyle(const String &status) {
  using namespace Theme;
  uint16_t c = status == "working" ? WORKING : status == "waiting" ? WAITING : status == "error" ? ERROR_ : status == "done" ? DONE : IDLE;
  const char *label = status == "working" ? "WORKING" : status == "waiting" ? "NEEDS YOU" : status == "error" ? "ERROR" : status == "done" ? "DONE" : "IDLE";
  uint8_t tint = status == "idle" ? 22 : 46;
  return {c, mix(SURFACE, c, tint), mix(SURFACE, c, 90), label};
}
inline String kindLabel(const String &kind) {
  if (kind == "claude") return "Claude";
  if (kind == "codex") return "Codex";
  if (kind == "opencode") return "OpenCode";
  String k = kind; if (k.length()) k.setCharAt(0, toupper(k[0])); return k;
}
inline String duration(uint32_t s) {
  if (s < 60) return String(s) + "s";
  if (s < 3600) return String(s / 60) + "m";
  if (s < 86400) return String(s / 3600) + "h " + String((s % 3600) / 60) + "m";
  return String(s / 86400) + "d " + String((s % 86400) / 3600) + "h";
}

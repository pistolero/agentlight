#pragma once
#include <Arduino.h>
#include <atomic>
#include <esp_ota_ops.h>
#include <esp_timer.h>

#ifndef FW_VERSION
#define FW_VERSION "dev"
#endif

// Shared OTA status. Written by the Wi-Fi task (core 0), read by the UI (core 1).
// Build identity embedded verbatim in the image; the helper scans the .bin for
// this marker to decide whether a display runs a different build.
extern "C" __attribute__((used)) const char AGENTLIGHT_BUILD[] = "AGENTLIGHT-BUILD:" FW_VERSION ";" __DATE__ " " __TIME__;

namespace Ota {
static const char *build() { return AGENTLIGHT_BUILD + 17; }
enum Phase : uint8_t { Idle, Preparing, Downloading, Verifying, Installing, Restarting, Failed };
static std::atomic<uint8_t> phase{Idle};
static std::atomic<uint32_t> received{0}, written{0}, total{0}, startedMs{0}, phaseMs{0};
static char newVersion[24] = "";
static char error[48] = "";
static bool pendingVerify = false;
static uint32_t bootMs = 0;
constexpr uint32_t CONFIRM_DEADLINE_MS = 180000;

static void setPhase(Phase p) { phaseMs = millis(); phase = p; }
static void fail(const char *reason) {
  strlcpy(error, reason, sizeof(error));
  setPhase(Failed);
}
static esp_timer_handle_t deadlineTimer = nullptr;
static void deadlineExpired(void *) {
  if (pendingVerify) esp_ota_mark_app_invalid_rollback_and_reboot();
}
// Call first in setup(): note whether this boot is an unconfirmed OTA image and
// arm a rollback timer that does not depend on the UI loop making progress.
static void begin() {
  bootMs = millis();
  esp_ota_img_states_t state;
  const esp_partition_t *running = esp_ota_get_running_partition();
  pendingVerify = running && esp_ota_get_state_partition(running, &state) == ESP_OK && state == ESP_OTA_IMG_PENDING_VERIFY;
  if (!pendingVerify) return;
  esp_timer_create_args_t args = {};
  args.callback = deadlineExpired;
  args.name = "ota-rollback";
  if (esp_timer_create(&args, &deadlineTimer) == ESP_OK) esp_timer_start_once(deadlineTimer, uint64_t(CONFIRM_DEADLINE_MS) * 1000);
}
// The new image proved it can reach an approved helper: keep it.
static void confirmIfPending() {
  if (!pendingVerify) return;
  if (esp_ota_mark_app_valid_cancel_rollback() == ESP_OK) {
    pendingVerify = false;
    if (deadlineTimer) esp_timer_stop(deadlineTimer);
  }
}
// An image that never reaches a helper is rolled back to the previous one.
static void checkDeadline() {
  if (pendingVerify && millis() - bootMs > CONFIRM_DEADLINE_MS) esp_ota_mark_app_invalid_rollback_and_reboot();
}
}

// Arduino marks a new image valid at boot unless this returns true; we confirm
// only after a successful helper session (see Ota::confirmIfPending).
extern "C" bool verifyRollbackLater() { return true; }

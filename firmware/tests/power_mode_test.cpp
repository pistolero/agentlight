// Host test for the sleep mode rules. From firmware/:
//   g++ -std=c++17 -Wall -Wextra -Iinclude tests/power_mode_test.cpp -o .pio/power_mode_test && .pio/power_mode_test
#include <cstdio>
#include <cstdlib>
#include "power_mode.h"

#define CHECK(c) do { if (!(c)) { printf("FAILED line %d: %s\n", __LINE__, #c); exit(1); } } while (0)
constexpr uint32_t T = SLEEP_TIMEOUT_MS;
using List = std::vector<AgentStatus>;
const List NONE = {};
const List A = {{"a", "waiting"}};
const List A_WORKING = {{"a", "working"}};
const List A_ERROR = {{"a", "error"}};
const List AB = {{"a", "waiting"}, {"b", "error"}};
const List B = {{"b", "error"}};

// Mode on and past the first frame, nothing needing attention.
PowerMode armed() { PowerMode p; p.statuses(NONE, 0); p.toggle(0); return p; }

int main() {
  { // Off: screen stays on, nothing alerts.
    PowerMode p; p.statuses(NONE, 0);
    p.tick(10 * T); CHECK(p.screenOn());
    CHECK(!p.statuses(A, 10 * T)); CHECK(!p.alerting());
  }
  { // Activating counts agents already waiting as seen.
    PowerMode p; p.statuses(A, 0); p.toggle(0);
    CHECK(p.active() && !p.alerting());
    CHECK(!p.statuses(A, 1)); CHECK(!p.alerting());
  }
  { // Timeout turns the screen off, not before.
    PowerMode p = armed();
    p.tick(T - 1); CHECK(p.screenOn());
    p.tick(T); CHECK(!p.screenOn()); CHECK(p.active());
  }
  { // A touch in the dark only wakes, keeps the mode, and restarts the timeout.
    PowerMode p = armed(); p.tick(T);
    CHECK(p.touch(T + 5)); CHECK(p.screenOn() && p.active());
    p.tick(2 * T + 4); CHECK(p.screenOn());
    p.tick(2 * T + 5); CHECK(!p.screenOn());
  }
  { // A touch with the screen on acts normally and restarts the timeout.
    PowerMode p = armed();
    CHECK(!p.touch(T - 1));
    p.tick(T); CHECK(p.screenOn());
  }
  { // A new agent needing attention wakes the screen and holds it on.
    PowerMode p = armed(); p.tick(T); CHECK(!p.screenOn());
    CHECK(p.statuses(A, T + 1)); CHECK(p.screenOn() && p.alerting());
    p.tick(100 * T); CHECK(p.screenOn());
  }
  { // Touch never dismisses an alert.
    PowerMode p = armed(); p.statuses(A, 1);
    CHECK(!p.touch(2)); CHECK(p.alerting());
    p.tick(100 * T); CHECK(p.screenOn());
  }
  { // waiting <-> error still needs attention: the alert continues, no new one.
    PowerMode p = armed(); p.statuses(A, 1);
    CHECK(!p.statuses(A_ERROR, 2)); CHECK(p.alerting());
  }
  { // Status change ends the alert; the screen then times out, mode still on.
    PowerMode p = armed(); p.statuses(A, 1);
    CHECK(!p.statuses(A_WORKING, 1000)); CHECK(!p.alerting() && p.screenOn() && p.active());
    p.tick(1000 + T - 1); CHECK(p.screenOn());
    p.tick(1000 + T); CHECK(!p.screenOn()); CHECK(p.active());
  }
  { // An agent closing ends its alert.
    PowerMode p = armed(); p.statuses(A, 1);
    p.statuses(NONE, 2); CHECK(!p.alerting());
  }
  { // With two alerting agents, the alert ends only when both have changed.
    PowerMode p = armed(); CHECK(p.statuses(AB, 1));
    p.statuses(B, 2); CHECK(p.alerting());
    p.statuses(NONE, 3); CHECK(!p.alerting());
  }
  { // A second agent during an alert extends it but doesn't report a new start,
    // so a log opened during the alert stays open.
    PowerMode p = armed(); CHECK(p.statuses(A, 1));
    CHECK(!p.statuses(AB, 2)); CHECK(p.alerting());
    p.statuses(B, 3); CHECK(p.alerting());
  }
  { // Re-activating during an alert re-arms: seen, counting down, mode still on.
    PowerMode p = armed(); p.statuses(A, 1);
    p.toggle(100); CHECK(p.active() && !p.alerting() && p.screenOn());
    CHECK(!p.statuses(A, 101)); CHECK(!p.alerting());
    p.tick(100 + T); CHECK(!p.screenOn());
  }
  { // Leaving attention and coming back alerts again, also after a re-arm.
    PowerMode p = armed(); p.statuses(A, 1); p.toggle(2);
    p.statuses(A_WORKING, 3); CHECK(p.statuses(A, 4)); CHECK(p.alerting());
  }
  { // The control in the counting-down state leaves the mode: screen stays on.
    PowerMode p = armed(); p.toggle(1);
    CHECK(!p.active()); p.tick(100 * T); CHECK(p.screenOn());
  }
  { // Leaving works from the dark too, once a touch has woken the screen.
    PowerMode p = armed(); p.tick(T); p.touch(T); p.toggle(T);
    CHECK(!p.active() && p.screenOn());
  }
  { // After a reboot the first frame is a baseline; later ones alert.
    PowerMode p; p.restore(true, 0);
    CHECK(p.active() && p.screenOn());
    CHECK(!p.statuses(A, 1)); CHECK(!p.alerting());
    CHECK(p.statuses(AB, 2)); CHECK(p.alerting());
  }
  { // Silence from the helper keeps the dark screen dark; attention raised while
    // it was gone alerts on the next frame.
    PowerMode p = armed(); p.tick(T);
    p.tick(50 * T); CHECK(!p.screenOn());
    CHECK(p.statuses(A, 50 * T)); CHECK(p.screenOn());
  }
  { // Waiting for a task is quiet; asking the user afterwards must alert.
    PowerMode p = armed(); p.tick(T);
    std::vector<AgentStatus> task = {{"task", "waiting_task"}};
    CHECK(!p.statuses(task, T + 1)); CHECK(!p.screenOn());
    std::vector<AgentStatus> ask = {{"task", "waiting"}};
    CHECK(p.statuses(ask, T + 2)); CHECK(p.screenOn());
    CHECK(!p.statuses(task, T + 3)); CHECK(!p.alerting());
  }
  puts("PASS: sleep mode rules");
}

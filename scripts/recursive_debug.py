#!/usr/bin/env python3
"""
Recursive Debugging Workflow for MamaXbox / Grand Theft Auto VI (Scarlett)
- Ensures zero dangling processes before launching.
- Monitors startup, detects when screen opens and presents frames.
- Automatically clicks A / X (via wtype Enter/Space, direct UDP pad packets, and VICTORY_AUTO_A).
- Waits until the menu loads.
- If the screen closes or the process exits unexpectedly, immediately kills associated tasks
  and extracts full diagnostic crash context.
"""

import os
import sys
import time
import signal
import socket
import struct
import subprocess
import re
from pathlib import Path

LOG_PATH = Path(os.path.expanduser("~/steam-7952987A.log"))
LAUNCHER = "/home/asiel/Games/launch-gta6-with-backend.sh"
PAD_PORT = 28777
BUTTON_A = 0x4
BUTTON_X = 0x10


def kill_all_processes():
    """Kill any running instance of mamaxbox, victory.exe, or launcher."""
    print("[Supervisor] Cleaning up lingering processes...", file=sys.stderr)
    patterns = [
        "launch-gta6-with-backend",
        "run-victory",
        "victory.exe",
        "game_scarlett",
    ]
    for pattern in patterns:
        subprocess.run(["pkill", "-9", "-f", pattern], stderr=subprocess.DEVNULL)
    
    # Clean wineserver in prefix
    wineserver_bin = Path(os.path.expanduser(
        "~/.local/share/Steam/steamapps/common/Proton - Experimental/files/bin/wineserver"
    ))
    pfx = Path(os.path.expanduser("~/.local/share/victory/proton-prefix"))
    if wineserver_bin.exists() and pfx.exists():
        try:
            subprocess.run(
                [str(wineserver_bin), "-k"],
                env={**os.environ, "WINEPREFIX": str(pfx)},
                stderr=subprocess.DEVNULL,
            )
        except Exception:
            pass
    time.sleep(1)


def send_pad_click(button: int):
    """Send UDP gamepad button pulse to 127.0.0.1:28777."""
    try:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        packet_down = struct.pack("<Iffffff", button, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0)
        packet_up = struct.pack("<Iffffff", 0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0)
        sock.sendto(packet_down, ("127.0.0.1", PAD_PORT))
        time.sleep(0.08)
        sock.sendto(packet_up, ("127.0.0.1", PAD_PORT))
    except Exception:
        pass


def send_wtype_key(key: str):
    """Send Wayland key event via wtype if available."""
    try:
        subprocess.run(["wtype", "-k", key], stderr=subprocess.DEVNULL, timeout=1)
    except Exception:
        pass


def focus_game_window():
    """Focus game window in Hyprland compositor."""
    try:
        out = subprocess.check_output(["hyprctl", "clients"], text=True)
        # Look for window title or class
        for line in out.splitlines():
            if "Grand Theft Auto VI" in line or "VIctory" in line:
                pass
        # Focus whatever is top or switch
        subprocess.run(["hyprctl", "dispatch", "focuswindow", "title:Grand Theft Auto VI"], stderr=subprocess.DEVNULL)
    except Exception:
        pass


def run_debug_cycle(max_wait_seconds=60):
    print("\n" + "=" * 65)
    print("=== Starting Recursive Debug Cycle for MamaXbox ===")
    print("=" * 65)

    kill_all_processes()

    # Clear log
    if LOG_PATH.exists():
        LOG_PATH.write_text("")

    env = os.environ.copy()
    env["VICTORY_AUTO_A"] = "1"
    env["VICTORY_TEXTURE_BUDGET_MB"] = "10240"
    env["VICTORY_TEXTURE_ROOM_PERCENT"] = "96"
    env["VICTORY_CARD_WINDOWS_MB"] = "4096"
    env["VICTORY_CARD_BUFFERS_MB"] = "3072"
    env["VICTORY_CARD_ROOM_FOR_TARGETS_MB"] = "1024"
    env["VICTORY_WINDOW_UPLOAD"] = "1"
    env["VICTORY_TEXTURE_SWEEP"] = "0"
    env["VICTORY_CARD_ACCOUNTING"] = "1"
    env["VICTORY_SKIP_PIPELINES"] = "4e296b1885c37b29,8bb272e763f76148,2486510c8afeef34"

    print(f"[Supervisor] Launching {LAUNCHER}...", file=sys.stderr)
    proc = subprocess.Popen([LAUNCHER], env=env, preexec_fn=os.setsid)

    screen_opened = False
    start_time = time.time()
    last_click_time = 0.0
    menu_loaded = False
    last_log_size = 0

    try:
        while time.time() - start_time < max_wait_seconds:
            # Check if process died
            ret = proc.poll()
            if ret is not None:
                print(f"[Supervisor] Process exited unexpectedly with code {ret}!", file=sys.stderr)
                break

            # Read new log lines
            if LOG_PATH.exists():
                text = LOG_PATH.read_text(errors="replace")
                new_text = text[last_log_size:]
                last_log_size = len(text)

                # Check for screen opened / first presented frame
                if not screen_opened and ("frame presented" in text or "frame=64" in text):
                    screen_opened = True
                    print(f"[Supervisor] Screen opened and presenting frames! Beginning A/X input clicks...", file=sys.stderr)

                # Check for menu loaded indications
                if screen_opened and ("menu" in text.lower() or "first world frame" in text or "frame=3000" in text):
                    print(f"[Supervisor] Menu / game flow loading confirmed!", file=sys.stderr)
                    menu_loaded = True

            # If screen is open, pulse A / X periodically
            now = time.time()
            if screen_opened and (now - last_click_time >= 1.5):
                last_click_time = now
                print(f"[Supervisor] Sending A/X input (wtype Return + UDP 0x4) at t+{now - start_time:.1f}s", file=sys.stderr)
                focus_game_window()
                send_pad_click(BUTTON_A)
                send_pad_click(BUTTON_X)
                send_wtype_key("Return")
                send_wtype_key("space")

            time.sleep(0.5)

    except KeyboardInterrupt:
        print("[Supervisor] Interrupted by user.", file=sys.stderr)
    finally:
        ret = proc.poll()
        if ret is not None or not menu_loaded:
            print("[Supervisor] Screen closed or timeout reached. Killing all associated tasks...", file=sys.stderr)
            kill_all_processes()
            try:
                os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
            except Exception:
                pass

    # Read final diagnostics
    if LOG_PATH.exists():
        lines = LOG_PATH.read_text(errors="replace").splitlines()
        print("\n--- Recent Log Tail (Last 30 lines) ---")
        for line in lines[-30:]:
            print(line)
        print("---------------------------------------\n")

    return menu_loaded


if __name__ == "__main__":
    success = run_debug_cycle()
    sys.exit(0 if success else 1)

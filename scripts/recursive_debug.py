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
        subprocess.run(["hyprctl", "dispatch", "focuswindow", "title:Americas"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        subprocess.run(["hyprctl", "dispatch", "focuswindow", "class:steam_proton"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    except Exception:
        pass


def get_game_window_geom():
    """Get (x, y, w, h) for the game window from hyprctl."""
    try:
        out = subprocess.check_output(["hyprctl", "clients"], text=True)
        is_game = False
        at = None
        size = None
        for line in out.splitlines():
            line_str = line.strip()
            if "Window " in line_str:
                is_game = False
                at = None
                size = None
            if "title: Americas" in line_str or "class: steam_proton" in line_str or "initialTitle: Americas" in line_str:
                is_game = True
            if line_str.startswith("at: "):
                parts = line_str[4:].split(",")
                at = (int(parts[0].strip()), int(parts[1].strip()))
            if line_str.startswith("size: "):
                parts = line_str[6:].split(",")
                size = (int(parts[0].strip()), int(parts[1].strip()))
            if is_game and at and size:
                return f"{at[0]},{at[1]} {size[0]}x{size[1]}"
    except Exception:
        pass
    return None


def run_debug_cycle(max_wait_seconds=180):
    print("\n" + "=" * 65)
    print("=== Starting Recursive Debug Cycle for MamaXbox ===")
    print("=" * 65)

    kill_all_processes()

    # Clear log
    if LOG_PATH.exists():
        LOG_PATH.write_text("")

    env = os.environ.copy()
    env["VICTORY_AUTO_A"] = "1"
    env["VICTORY_FPS_LIMIT"] = "60"
    env["VICTORY_STACKS"] = "20,35"
    env["VICTORY_WALLS_TRACE"] = "1"
    env["VICTORY_TEXTURE_BUDGET_MB"] = "10240"
    env["VICTORY_TEXTURE_ROOM_PERCENT"] = "96"
    env["VICTORY_CARD_WINDOWS_MB"] = "4096"
    env["VICTORY_CARD_BUFFERS_MB"] = "4096"
    env["VICTORY_CARD_ROOM_FOR_TARGETS_MB"] = "1024"
    env["VICTORY_WINDOW_UPLOAD"] = "1"
    env["VICTORY_TEXTURE_SWEEP"] = "0"
    env["VICTORY_CARD_ACCOUNTING"] = "1"
    env["VICTORY_QUEUE2"] = "3"
    env["VICTORY_QUEUE2_LANDING_MB"] = "256"
    env["VKD3D_CONFIG"] = "upload_hvv"
    env["__GL_THREADED_OPTIMIZATIONS"] = "1"
    env["VICTORY_PIPELINE_WORKERS"] = "8"
    env["VICTORY_DRIVER_COMPILE_LOCK"] = "1"
    env["VICTORY_MESH_LAUNCH"] = "1"
    env["VICTORY_CARD_HASH_WIDE"] = "1"
    env["VICTORY_DIRECT_CONSTANTS"] = "1"
    env["VICTORY_REBAR_COPIES_MB"] = "2048"
    env["VICTORY_PAGE_MAPPING_RUNS"] = "1"
    env["VICTORY_PERSISTENT_VIEWS"] = "1"
    env["VICTORY_BATCHED_RESIDENCY"] = "1"
    env["VICTORY_ASYNC_SUBMIT"] = "1"
    env["VICTORY_ASYNC_PRESENT"] = "1"
    env["VICTORY_PIPELINE_STANDINS"] = "1"
    env["VICTORY_PIPELINE_WAIT_MS"] = "4"
    env["VICTORY_PIPELINE_WAIT_ONCE_MS"] = "500"
    env["VICTORY_SKIP_PIPELINES"] = "4e296b1885c37b29,8bb272e763f76148,2486510c8afeef34,f607be37ee4fba77,656fdcac1e85f2ea"

    print(f"[Supervisor] Launching {LAUNCHER}...", file=sys.stderr)
    proc = subprocess.Popen([LAUNCHER], env=env, preexec_fn=os.setsid)

    screen_opened = False
    start_time = time.time()
    last_click_time = 0.0
    last_shot_time = 0.0
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

                # Check for driver crashes or compiler errors in new text
                for line in new_text.splitlines():
                    if "driver shader compiler crashed" in line or "SIGILL" in line:
                        print(f"[Supervisor CRASH] {line}", file=sys.stderr)

                # Check for true world frame loading
                if screen_opened and ("story000" in text or "frame=2048" in text or "frame=4096" in text or "300 draws/frame" in text):
                    if not menu_loaded:
                        print(f"[Supervisor] Advanced into story000 world streaming!", file=sys.stderr)
                        menu_loaded = True

            now = time.time()
            # If screen is open, monitor GPU status periodically
            if screen_opened and (now - last_shot_time >= 5.0):
                last_shot_time = now
                try:
                    smi = subprocess.check_output(
                        ["nvidia-smi", "--query-gpu=utilization.gpu,memory.used,memory.total,power.draw,temperature.gpu", "--format=csv,noheader,nounits"],
                        text=True,
                        timeout=2,
                    ).strip()
                    parts = [p.strip() for p in smi.split(",")]
                    if len(parts) >= 5:
                        print(f"[GPU Monitor t+{now - start_time:.0f}s] GPU Util: {parts[0]}% | VRAM: {parts[1]}MB / {parts[2]}MB | Power: {parts[3]}W | Temp: {parts[4]}°C", file=sys.stderr)
                except Exception:
                    pass
                try:
                    geom = get_game_window_geom()
                    if geom:
                        subprocess.run(["grim", "-g", geom, "/tmp/gta6_screen_latest.png"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                    else:
                        subprocess.run(["grim", "/tmp/gta6_screen_latest.png"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                    
                    # Copy latest capture to conversation artifact directory
                    artifact_path = Path("/home/asiel/.gemini/antigravity-cli/brain/7e79563a-32f0-4488-843e-b0f24670ac94/gta6_game_screen.png")
                    if Path("/tmp/gta6_screen_latest.png").exists():
                        import shutil
                        shutil.copyfile("/tmp/gta6_screen_latest.png", artifact_path)

                    subprocess.run(["tesseract", "/tmp/gta6_screen_latest.png", "/tmp/gta6_ocr"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                    ocr_text = Path("/tmp/gta6_ocr.txt").read_text(errors="replace").strip()
                    for line in ocr_text.splitlines():
                        if "SCARLETT HUD" in line or "FPS" in line:
                            print(f"[HUD t+{now - start_time:.0f}s] {line.strip()}", file=sys.stderr)
                    cleaned_lines = [l.strip() for l in ocr_text.splitlines() if l.strip() and not l.startswith("SCARLETT HUD")]
                    if cleaned_lines:
                        sample = " | ".join(cleaned_lines[:4])
                        print(f"[Screen OCR t+{now - start_time:.0f}s] {sample}", file=sys.stderr)
                except Exception:
                    pass

            # If screen is open, pulse A / X periodically
            if screen_opened and (now - last_click_time >= 1.5):
                last_click_time = now
                if int(now - start_time) % 15 == 0:
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

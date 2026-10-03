#!/usr/bin/env python3
"""Copy selected, freshly rendered GUI review images into the documentation."""
from pathlib import Path
import shutil

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "dist/gui-review"
DESTINATION = ROOT / "docs/screenshots/desktop"
IMAGES = {
    **{f"{view}.png": f"macbook-default-{view}.png"
       for view in ("dashboard", "connect", "transfer", "upgrade", "logs")},
    "interfaces.png": "header-dark-interfaces.png",
    "settings.png": "header-light-settings.png",
    "cli.png": "cli-desktop.png",
    "workflow.png": "wizard-macbook-step-3.png",
    "jobs.png": "jobs.png",
    "light-dashboard.png": "light-dashboard.png",
}


def main():
    missing = [name for name in IMAGES.values() if not (SOURCE / name).is_file()]
    if missing:
        raise SystemExit(f"Render desktop_preview first. Missing: {', '.join(missing)}")
    DESTINATION.mkdir(parents=True, exist_ok=True)
    for destination, source in IMAGES.items():
        shutil.copy2(SOURCE / source, DESTINATION / destination)
        print(DESTINATION / destination)


if __name__ == "__main__":
    main()

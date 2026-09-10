import { useRef, useState, type PointerEvent } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import codexbarIcon from "../assets/codexbar-icon.png";
import { refreshProvidersIfStale } from "../lib/tauri";
import { toggleFloatingQuota } from "../floating-quota/api";

type PointerState = {
  pointerId: number;
  startX: number;
  startY: number;
  dragging: boolean;
};

export default function FloatingBall() {
  const pointer = useRef<PointerState | null>(null);
  const [pressed, setPressed] = useState(false);

  const onPointerDown = (event: PointerEvent<HTMLButtonElement>) => {
    pointer.current = {
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      dragging: false,
    };
    event.currentTarget.setPointerCapture?.(event.pointerId);
    setPressed(true);
  };

  const onPointerMove = (event: PointerEvent<HTMLButtonElement>) => {
    const current = pointer.current;
    if (!current || current.pointerId !== event.pointerId || current.dragging) return;

    const distance = Math.hypot(
      event.clientX - current.startX,
      event.clientY - current.startY,
    );
    if (distance <= 4) return;

    current.dragging = true;
    setPressed(false);
    void getCurrentWindow().startDragging();
  };

  const finishPointer = () => {
    const shouldToggle = pointer.current !== null && !pointer.current.dragging;
    pointer.current = null;
    setPressed(false);
    if (shouldToggle) {
      void toggleFloatingQuota()
        .then((shown) => {
          if (shown) return refreshProvidersIfStale();
          return undefined;
        })
        .catch(() => {});
    }
  };

  const cancelPointer = () => {
    pointer.current = null;
    setPressed(false);
  };

  return (
    <button
      type="button"
      aria-label="CodexBar"
      className={`floating-ball${pressed ? " is-pressed" : ""}`}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={finishPointer}
      onPointerCancel={cancelPointer}
    >
      <img src={codexbarIcon} alt="" draggable={false} />
    </button>
  );
}

import React from "react";
import { useCurrentFrame, interpolate } from "remotion";
import { TerminalWindow } from "../Terminal/TerminalWindow";
import { TuiLayout } from "../Terminal/TuiLayout";
import { COLORS } from "../../data/brand";
import { getStateAtFrame, FRAMES } from "../../data/tui-states";

export const ScenePipeline: React.FC = () => {
  const frame = useCurrentFrame();
  // Offset: this scene starts at FRAMES.REVEAL_END in absolute terms
  const absoluteFrame = FRAMES.REVEAL_END + frame;
  const state = getStateAtFrame(absoluteFrame);

  // Camera: spring toward state's cameraScale / cameraY
  // Simple interpolate for camera (smooth but not spring — avoids oscillation)
  // We compute a 0–1 progress toward the confirm widget appearance
  const confirmRelFrame = FRAMES.CONFIRM_APPEAR - FRAMES.REVEAL_END;
  const cameraProgress = interpolate(
    frame,
    [confirmRelFrame - 5, confirmRelFrame + 30],
    [0, 1],
    { extrapolateLeft: "clamp", extrapolateRight: "clamp" }
  );
  // And back after accept
  const acceptRelFrame = FRAMES.ACCEPT_FRAME - FRAMES.REVEAL_END;
  const cameraReturnProgress = interpolate(
    frame,
    [acceptRelFrame, acceptRelFrame + 30],
    [0, 1],
    { extrapolateLeft: "clamp", extrapolateRight: "clamp" }
  );

  const cameraScale =
    frame < confirmRelFrame
      ? 1.0
      : frame < acceptRelFrame
        ? interpolate(cameraProgress, [0, 1], [1.0, 1.15])
        : interpolate(cameraReturnProgress, [0, 1], [1.15, 1.0]);

  const cameraY =
    frame < confirmRelFrame
      ? 0
      : frame < acceptRelFrame
        ? interpolate(cameraProgress, [0, 1], [0, -40])
        : interpolate(cameraReturnProgress, [0, 1], [-40, 0]);

  // Border glow: ramps up as steps complete
  const completedCount = state.steps.filter(
    (s) => s.status === "success" || s.status === "warning"
  ).length;
  const glowIntensity = (completedCount / 5) * 0.4; // max 0.4 during pipeline

  return (
    <div
      style={{
        width: "100%",
        height: "100%",
        background: COLORS.background,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
      }}
    >
      <div
        style={{
          transform: `scale(${cameraScale}) translateY(${cameraY}px)`,
          transition: "none", // Remotion handles frame-by-frame, no CSS transitions
        }}
      >
        <TerminalWindow width={1200} height={640} glowIntensity={glowIntensity}>
          <TuiLayout
            steps={state.steps}
            activeStepIndex={state.activeStepIndex}
            outputLines={state.outputLines}
            confirmWidget={state.confirmWidget}
            currentFrame={absoluteFrame}
          />
        </TerminalWindow>
      </div>
    </div>
  );
};

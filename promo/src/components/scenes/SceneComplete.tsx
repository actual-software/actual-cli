import React from "react";
import { useCurrentFrame, spring, interpolate } from "remotion";
import { TerminalWindow } from "../Terminal/TerminalWindow";
import { TuiLayout } from "../Terminal/TuiLayout";
import { COLORS, SPRING_CONFIGS } from "../../data/brand";
import { getStateAtFrame, FRAMES } from "../../data/tui-states";

interface SceneCompleteProps {
  // When the preceding scene ends zoomed in, pass that scale here so
  // SceneComplete smoothly zooms out over the first 40 frames instead of
  // jumping to 1.0. HeroClip doesn't need this (ScenePipeline already
  // handles the zoom-out). ShortClip passes 1.15 here.
  initialScale?: number;
}

export const SceneComplete: React.FC<SceneCompleteProps> = ({ initialScale = 1.0 }) => {
  const frame = useCurrentFrame();
  const absoluteFrame = FRAMES.WRITE_END + frame;
  const state = getStateAtFrame(absoluteFrame);

  // If the preceding scene ended zoomed in, zoom out over the first 40 frames.
  const zoomOut =
    initialScale > 1.0
      ? interpolate(frame, [0, 40], [initialScale, 1.0], {
          extrapolateLeft: "clamp",
          extrapolateRight: "clamp",
        })
      : 1.0;

  // Breathe out: scale 1.0 → 0.98 → 1.0 (subtle)
  const breathe = Math.sin((frame / 300) * Math.PI); // 0 → 1 → 0 over 300 frames
  const cameraScale = zoomOut - breathe * 0.02;

  // Glow: peaks at frame 60, then settles
  const glowPeak = spring({
    frame,
    fps: 60,
    config: SPRING_CONFIGS.glowBurst,
    durationInFrames: 60,
  });
  const glowIntensity = Math.min(glowPeak, 1.3) * 0.8; // cap at 1.04 → * 0.8 = 0.83

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
      <div style={{ transform: `scale(${cameraScale})` }}>
        <TerminalWindow width={1200} height={640} glowIntensity={glowIntensity}>
          <TuiLayout
            steps={state.steps}
            activeStepIndex={state.activeStepIndex}
            outputLines={state.outputLines}
            currentFrame={absoluteFrame}
          />
        </TerminalWindow>
      </div>
    </div>
  );
};

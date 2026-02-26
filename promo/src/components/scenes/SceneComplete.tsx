import React from "react";
import { useCurrentFrame, spring, interpolate } from "remotion";
// Note: `spring` is used for glowIntensity only; no breathe effect (it was a
// sin wave over 300 frames that SceneComplete never completed — it just slowly
// shrank the terminal by <2%, causing a visible scale pop at the Seq→CTA cut).
import { TerminalWindow } from "../Terminal/TerminalWindow";
import { TuiLayout } from "../Terminal/TuiLayout";
import { COLORS, SPRING_CONFIGS } from "../../data/brand";
import { getStateAtFrame, FRAMES } from "../../data/tui-states";

interface SceneCompleteProps {
  // When the preceding scene ends zoomed in/translated, pass the ending
  // camera state so SceneComplete smoothly transitions over the first 40
  // frames instead of jumping. HeroClip doesn't need this (ScenePipeline
  // already completes the zoom-out). SocialMediaClip passes 1.15 / -40.
  initialScale?: number;
  initialOffsetY?: number;
}

export const SceneComplete: React.FC<SceneCompleteProps> = ({
  initialScale = 1.0,
  initialOffsetY = 0,
}) => {
  const frame = useCurrentFrame();
  const absoluteFrame = FRAMES.WRITE_END + frame;
  const state = getStateAtFrame(absoluteFrame);

  const transitioning = initialScale > 1.0 || initialOffsetY !== 0;

  // If the preceding scene ended zoomed in / shifted, ease both back to
  // neutral over 40 frames.
  const zoomOut = transitioning
    ? interpolate(frame, [0, 40], [initialScale, 1.0], {
        extrapolateLeft: "clamp",
        extrapolateRight: "clamp",
      })
    : 1.0;

  const cameraY = transitioning
    ? interpolate(frame, [0, 40], [initialOffsetY, 0], {
        extrapolateLeft: "clamp",
        extrapolateRight: "clamp",
      })
    : 0;

  const cameraScale = zoomOut;

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
      <div style={{ transform: `scale(${cameraScale}) translateY(${cameraY}px)` }}>
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

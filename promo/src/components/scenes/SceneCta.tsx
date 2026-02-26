import React from "react";
import { useCurrentFrame, interpolate, spring } from "remotion";
import { TerminalWindow } from "../Terminal/TerminalWindow";
import { TuiLayout } from "../Terminal/TuiLayout";
import { COLORS, FONTS, SPRING_CONFIGS } from "../../data/brand";
import { COPY } from "../../data/copy";
import { getStateAtFrame, FRAMES } from "../../data/tui-states";

function GradientText({
  text,
  fontSize,
}: {
  text: string;
  fontSize: number;
}) {
  return (
    <span
      style={{
        fontFamily: FONTS.mono,
        fontSize,
        fontWeight: 700,
        background: `linear-gradient(135deg, ${COLORS.borderGreen}, ${COLORS.borderTeal})`,
        WebkitBackgroundClip: "text",
        WebkitTextFillColor: "transparent",
        backgroundClip: "text",
        display: "inline-block",
      }}
    >
      {text}
    </span>
  );
}

interface SceneCtaProps {
  /** Total duration of this sequence in frames. Fadeout fires in the last 30f. */
  totalDuration?: number;
}

export const SceneCta: React.FC<SceneCtaProps> = ({ totalDuration = 180 }) => {
  const frame = useCurrentFrame();
  const absoluteFrame = FRAMES.CTA_START + frame;
  const state = getStateAtFrame(absoluteFrame);

  // 0.5s static hold on the completed TUI before any animation begins
  const HOLD = 30;
  const animFrame = Math.max(0, frame - HOLD);

  // Terminal slides left and shrinks
  const slideProgress = spring({
    frame: animFrame,
    fps: 60,
    config: SPRING_CONFIGS.settle,
    durationInFrames: 40,
  });
  const termX = interpolate(slideProgress, [0, 1], [0, -280]);
  const termScale = interpolate(slideProgress, [0, 1], [1.0, 0.62]);

  // Right side content fades in with stagger (all offset by HOLD)
  const wordmarkOpacity = interpolate(frame, [HOLD + 20, HOLD + 45], [0, 1], {
    extrapolateRight: "clamp",
  });
  const taglineOpacity = interpolate(frame, [HOLD + 45, HOLD + 70], [0, 1], {
    extrapolateRight: "clamp",
  });
  const urlOpacity = interpolate(frame, [HOLD + 70, HOLD + 90], [0, 1], {
    extrapolateRight: "clamp",
  });

  // URL underline grows left-to-right
  const urlUnderlineWidth = interpolate(frame, [HOLD + 90, HOLD + 130], [0, 100], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });

  // Final fade to black: last 30 frames of the sequence
  const fadeOut = interpolate(
    frame,
    [totalDuration - 30, totalDuration],
    [1, 0],
    { extrapolateLeft: "clamp", extrapolateRight: "clamp" }
  );

  return (
    <div
      style={{
        width: "100%",
        height: "100%",
        background: COLORS.background,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        opacity: fadeOut,
      }}
    >
      {/* Terminal — left third, slides in */}
      <div
        style={{
          position: "absolute",
          left: "50%",
          top: "50%",
          transform: `translate(calc(-50% + ${termX}px), -50%) scale(${termScale})`,
          transformOrigin: "center center",
        }}
      >
        <TerminalWindow width={1200} height={640} glowIntensity={0.2}>
          <TuiLayout
            steps={state.steps}
            activeStepIndex={state.activeStepIndex}
            outputLines={state.outputLines}
            currentFrame={absoluteFrame}
          />
        </TerminalWindow>
      </div>

      {/* Right side CTA content */}
      <div
        style={{
          position: "absolute",
          right: "10%",
          top: "50%",
          transform: "translateY(-50%)",
          display: "flex",
          flexDirection: "column",
          gap: 16,
          maxWidth: 500,
        }}
      >
        {/* Wordmark */}
        <div style={{ opacity: wordmarkOpacity }}>
          <GradientText text={COPY.cta.wordmark} fontSize={72} />
        </div>

        {/* Tagline */}
        <div
          style={{
            opacity: taglineOpacity,
            fontFamily: FONTS.mono,
            fontSize: 18,
            color: COLORS.textPrimary,
            lineHeight: 1.5,
          }}
        >
          {COPY.cta.tagline}
        </div>

        {/* URL with animated line extending to the right */}
        <div
          style={{
            opacity: urlOpacity,
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            gap: 12,
            width: "100%",
          }}
        >
          <span
            style={{
              fontFamily: FONTS.mono,
              fontSize: 20,
              color: COLORS.borderGreen,
              letterSpacing: 1,
              flexShrink: 0,
            }}
          >
            {COPY.cta.url}
          </span>
          <div
            style={{
              flex: 1,
              height: 2,
              marginRight: -200,
              background: `linear-gradient(90deg, ${COLORS.borderGreen}, ${COLORS.borderTeal})`,
              transformOrigin: "left center",
              transform: `scaleX(${urlUnderlineWidth / 100})`,
            }}
          />
        </div>
      </div>
    </div>
  );
};

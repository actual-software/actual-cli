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

export const SceneCta: React.FC = () => {
  const frame = useCurrentFrame();
  const state = getStateAtFrame(FRAMES.CTA_START + frame);

  // Terminal slides left and shrinks
  const slideProgress = spring({
    frame,
    fps: 60,
    config: SPRING_CONFIGS.settle,
    durationInFrames: 40,
  });
  const termX = interpolate(slideProgress, [0, 1], [0, -280]);
  const termScale = interpolate(slideProgress, [0, 1], [1.0, 0.62]);

  // Right side content fades in with stagger
  const wordmarkOpacity = interpolate(frame, [20, 45], [0, 1], {
    extrapolateRight: "clamp",
  });
  const taglineOpacity = interpolate(frame, [45, 70], [0, 1], {
    extrapolateRight: "clamp",
  });
  const urlOpacity = interpolate(frame, [70, 90], [0, 1], {
    extrapolateRight: "clamp",
  });

  // URL underline grows left-to-right
  const urlUnderlineWidth = interpolate(frame, [90, 130], [0, 100], {
    extrapolateRight: "clamp",
  });

  // Final fade to black after frame 150
  const fadeOut = interpolate(frame, [150, 180], [1, 0], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });

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
          />
        </TerminalWindow>
      </div>

      {/* Right side CTA content */}
      <div
        style={{
          position: "absolute",
          right: "8%",
          top: "50%",
          transform: "translateY(-50%)",
          display: "flex",
          flexDirection: "column",
          gap: 16,
          maxWidth: 420,
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

        {/* URL with animated underline */}
        <div
          style={{ opacity: urlOpacity, position: "relative", display: "inline-block" }}
        >
          <span
            style={{
              fontFamily: FONTS.mono,
              fontSize: 20,
              color: COLORS.borderGreen,
              letterSpacing: 1,
            }}
          >
            {COPY.cta.url}
          </span>
          <div
            style={{
              position: "absolute",
              bottom: -2,
              left: 0,
              height: 2,
              width: `${urlUnderlineWidth}%`,
              background: `linear-gradient(90deg, ${COLORS.borderGreen}, ${COLORS.borderTeal})`,
            }}
          />
        </div>
      </div>
    </div>
  );
};

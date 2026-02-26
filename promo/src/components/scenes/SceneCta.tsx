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
  /** Layout variant:
   *  "wide"     = 16:9 — terminal slides left, CTA appears on the right.
   *  "square"   = 1:1  — sandwich: wordmark top band, terminal middle, CTA bottom band.
   *  "portrait" = 9:16 — sandwich: wordmark top band (384px), terminal middle, CTA bottom band (384px).
   */
  layout?: "wide" | "square" | "portrait";
  /** Horizontal offset (px) applied to the terminal in square layout — should match the pipeline scene shift. */
  terminalOffsetX?: number;
  /** Portrait layout: vertical offset from canvas top for the terminal (px). Matches PORTRAIT_TERM_TOP. */
  portraitTermTop?: number;
  /** Portrait layout: horizontal offset from canvas left for the terminal (px). Matches PORTRAIT_TERM_LEFT. */
  portraitTermLeft?: number;
  /** Portrait layout: CSS scale applied to the 1920×1080 block. Matches PORTRAIT_SCENE_SCALE. */
  portraitScale?: number;
}

export const SceneCta: React.FC<SceneCtaProps> = ({
  totalDuration = 180,
  layout = "wide",
  terminalOffsetX = 0,
  portraitTermTop = 384,
  portraitTermLeft = 20,
  portraitScale = 1.8,
}) => {
  const frame = useCurrentFrame();
  const absoluteFrame = FRAMES.CTA_START + frame;
  const state = getStateAtFrame(absoluteFrame);

  // 0.5s static hold on the completed TUI before any animation begins
  const HOLD = 30;
  const animFrame = Math.max(0, frame - HOLD);

  // Glow decays from SceneComplete's settled level (0.8) to the CTA resting
  // level (0.2) over the HOLD window, so the cut is seamless.
  const terminalGlow = interpolate(frame, [0, HOLD], [0.8, 0.2], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });

  // Terminal slides left and shrinks (wide layout only)
  const slideProgress = spring({
    frame: animFrame,
    fps: 60,
    config: SPRING_CONFIGS.settle,
    durationInFrames: 40,
  });
  const termX = interpolate(slideProgress, [0, 1], [0, -280]);
  const termScale = interpolate(slideProgress, [0, 1], [1.0, 0.62]);

  // CTA content fades in with stagger (all offset by HOLD)
  const wordmarkOpacity = interpolate(frame, [HOLD + 20, HOLD + 45], [0, 1], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });
  const taglineOpacity = interpolate(frame, [HOLD + 45, HOLD + 70], [0, 1], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });
  const urlOpacity = interpolate(frame, [HOLD + 70, HOLD + 90], [0, 1], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });

  // URL line grows left-to-right
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

  // Shared URL row (used by all layouts)
  const urlRow = (
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
  );

  // ── Portrait layout (9:16) ─────────────────────────────────────────────────
  // Sandwich layout (mirrors square):
  //   Top band  (portraitTermTop px, default 384): wordmark left-aligned.
  //   Middle:   terminal at (portraitTermLeft, portraitTermTop) scaled by
  //             portraitScale with transformOrigin "top left" — exactly matches
  //             the pipeline scene block position so there is no jump on cut.
  //             The terminal is rendered from the 1920×1080 block origin, so
  //             we position the raw 1200×640 TerminalWindow and apply the same
  //             scale. The pipeline block also applies SQUARE_TERM_OFFSET before
  //             scaling, which shifts the block origin — the terminal's canvas-px
  //             left edge is simply portraitTermLeft.
  //   Bottom band (portraitTermTop px, default 384): tagline + URL row.
  if (layout === "portrait") {
    return (
      <div
        style={{
          width: "100%",
          height: "100%",
          background: COLORS.background,
          position: "relative",
          overflow: "hidden",
          opacity: fadeOut,
        }}
      >
        {/* Terminal — same transform as the pipeline block so there is no
            positional jump on the Seq3 → Seq4 cut.
            The block is a 1920×1080 div scaled by portraitScale with origin
            top-left and offset by (PORTRAIT_BLOCK_TX, PORTRAIT_BLOCK_TY).
            Inside it the SQUARE_TERM_OFFSET shift positions the terminal.
            Net canvas position of the terminal's top-left corner:
              x = portraitTermLeft, y = portraitTermTop  (by construction). */}
        <div
          style={{
            position: "absolute",
            left: portraitTermLeft,
            top: portraitTermTop,
            width: 1200,
            height: 640,
            transform: `scale(${portraitScale})`,
            transformOrigin: "top left",
          }}
        >
          <TerminalWindow width={1200} height={640} glowIntensity={terminalGlow}>
            <TuiLayout
              steps={state.steps}
              activeStepIndex={state.activeStepIndex}
              outputLines={state.outputLines}
              currentFrame={absoluteFrame}
            />
          </TerminalWindow>
        </div>

        {/* Top band — wordmark (matches square layout) */}
        <div
          style={{
            position: "absolute",
            top: 0,
            left: 0,
            right: 0,
            height: portraitTermTop,
            display: "flex",
            alignItems: "center",
            justifyContent: "flex-start",
            paddingLeft: 60,
            opacity: wordmarkOpacity,
          }}
        >
          <GradientText text={COPY.cta.wordmark} fontSize={72} />
        </div>

        {/* Bottom band — tagline + URL row (matches square layout) */}
        <div
          style={{
            position: "absolute",
            bottom: 0,
            left: 0,
            right: 0,
            height: portraitTermTop,
            display: "flex",
            flexDirection: "column",
            justifyContent: "center",
            padding: "0 60px",
            gap: 14,
          }}
        >
          <div
            style={{
              opacity: taglineOpacity,
              fontFamily: FONTS.mono,
              fontSize: 16,
              color: COLORS.textPrimary,
              lineHeight: 1.5,
            }}
          >
            {COPY.cta.tagline}
          </div>
          {urlRow}
        </div>
      </div>
    );
  }

  // ── Square layout (1:1) ─────────────────────────────────────────────────────
  // Terminal matches the 1920px-centred position used by the pipeline scenes so
  // there is no positional jump on the cut. Top/bottom bands fill the canvas.
  if (layout === "square") {
    return (
      <div
        style={{
          width: "100%",
          height: "100%",
          background: COLORS.background,
          position: "relative",
          overflow: "hidden",
          opacity: fadeOut,
        }}
      >
        {/* Terminal — 1920px-wide inner container mirrors the pipeline centering + offset */}
        <div
          style={{
            position: "absolute",
            left: terminalOffsetX,
            top: 0,
            width: 1920,
            height: "100%",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
          }}
        >
          <TerminalWindow width={1200} height={640} glowIntensity={terminalGlow}>
            <TuiLayout
              steps={state.steps}
              activeStepIndex={state.activeStepIndex}
              outputLines={state.outputLines}
              currentFrame={absoluteFrame}
            />
          </TerminalWindow>
        </div>

        {/* Top band — wordmark */}
        <div
          style={{
            position: "absolute",
            top: 0,
            left: 0,
            right: 0,
            height: 220,
            display: "flex",
            alignItems: "center",
            justifyContent: "flex-start",
            paddingLeft: 60,
            opacity: wordmarkOpacity,
          }}
        >
          <GradientText text={COPY.cta.wordmark} fontSize={72} />
        </div>

        {/* Bottom band — tagline + URL+line */}
        <div
          style={{
            position: "absolute",
            bottom: 0,
            left: 0,
            right: 0,
            height: 220,
            display: "flex",
            flexDirection: "column",
            justifyContent: "center",
            padding: "0 60px",
            gap: 14,
          }}
        >
          <div
            style={{
              opacity: taglineOpacity,
              fontFamily: FONTS.mono,
              fontSize: 16,
              color: COLORS.textPrimary,
              lineHeight: 1.5,
            }}
          >
            {COPY.cta.tagline}
          </div>
          {urlRow}
        </div>
      </div>
    );
  }

  // ── Wide layout (16:9, default) ─────────────────────────────────────────────
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
      {/* Terminal — slides left and shrinks */}
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

        {urlRow}
      </div>
    </div>
  );
};

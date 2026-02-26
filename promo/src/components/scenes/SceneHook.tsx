import React from "react";
import { useCurrentFrame, spring, interpolate } from "remotion";
import { TerminalWindow } from "../Terminal/TerminalWindow";
import { COLORS, FONTS, SPRING_CONFIGS } from "../../data/brand";
import { COPY } from "../../data/copy";

const COMMAND = COPY.shell.command; // "actual adr-bot"
const PROMPT = COPY.shell.prompt; // "~/my-project  on  main"

export const SceneHook: React.FC = () => {
  const frame = useCurrentFrame();

  // Terminal slides up with spring
  const slideUp = spring({
    frame,
    fps: 60,
    config: SPRING_CONFIGS.slideIn,
    durationInFrames: 40,
  });
  const translateY = interpolate(slideUp, [0, 1], [60, 0]);

  // Opacity fade in
  const opacity = interpolate(frame, [0, 15], [0, 1], {
    extrapolateRight: "clamp",
  });

  // Typing: 1 char per 4 frames (≈67ms/char at 60fps)
  const TYPING_START = 30; // frame when typing begins
  const CHARS_PER_FRAME = 1 / 4;
  const charsTyped = Math.floor(
    Math.max(0, frame - TYPING_START) * CHARS_PER_FRAME
  );
  const typedText = COMMAND.slice(0, charsTyped);
  const showCursor = frame < 170; // cursor hides just before Enter

  // Cursor blink: 30 frames on, 30 frames off
  const cursorVisible = showCursor && Math.floor(frame / 15) % 2 === 0;

  return (
    <div
      style={{
        width: "100%",
        height: "100%",
        background: COLORS.background,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        opacity,
        transform: `translateY(${translateY}px)`,
      }}
    >
      <TerminalWindow width={1200} height={640}>
        <div
          style={{
            fontFamily: FONTS.mono,
            fontSize: 14,
            color: COLORS.textPrimary,
            padding: "24px 20px",
            display: "flex",
            flexDirection: "column",
            gap: 0,
          }}
        >
          {/* Prompt line */}
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <span style={{ color: COLORS.borderGreen }}>{PROMPT}</span>
            <span style={{ color: COLORS.textDim }}>❯</span>
            <span style={{ color: COLORS.textPrimary, marginLeft: 4 }}>
              {typedText}
            </span>
            {cursorVisible && (
              <span
                style={{
                  display: "inline-block",
                  width: "0.6ch",
                  height: "1.1em",
                  background: COLORS.textPrimary,
                  marginLeft: 1,
                  verticalAlign: "middle",
                }}
              />
            )}
          </div>
        </div>
      </TerminalWindow>
    </div>
  );
};

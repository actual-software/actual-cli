import React from "react";
import { COLORS, FONTS } from "../../data/brand";
import { LogoPanel } from "./LogoPanel";
import { StepsPanel, StepDef } from "./StepsPanel";
import { OutputPane } from "./OutputPane";
import { OutputLine, ConfirmWidgetState } from "../../data/tui-states";

interface TuiLayoutProps {
  steps: StepDef[];
  activeStepIndex: number;
  outputLines: OutputLine[];
  confirmWidget?: ConfirmWidgetState;
  glowIntensity?: number;
  /** Pass absoluteFrame from remapped clips (LoopClip, FastPipeline) so that
   *  OutputPane/StepsPanel/Spinner compare against correct absolute frame numbers. */
  currentFrame?: number;
}

// Matches the ratatui gradient.rs: linear lerp from #00FB7E (top) → #179CA9 (bottom)
// over the full frame height. t=0 is the top of the TUI, t=1 is the bottom.
function gradientColor(t: number): string {
  const r = Math.round(0x00 + t * (0x17 - 0x00));
  const g = Math.round(0xfb + t * (0x9c - 0xfb));
  const b = Math.round(0x7e + t * (0xa9 - 0x7e));
  return `rgb(${r},${g},${b})`;
}

// Returns a CSS linear-gradient for a box whose top edge is at fraction y0
// and bottom edge is at fraction y1 within the full TUI content area.
// This replicates ratatui's "t computed against full frame height" behaviour.
function borderGradient(y0: number, y1: number): string {
  return `linear-gradient(to bottom, ${gradientColor(y0)}, ${gradientColor(y1)})`;
}

// Ratatui-style bordered panel using the gradient-outer / solid-inner trick.
// The 2px "border" is rendered as the exposed gradient background behind
// a solid-surfaced inner div. Supports border-radius unlike border-image.
// Titles protrude ±8px via position:absolute — NO overflow:hidden on the
// outer div so they are not clipped (TuiLayout's padded outer div clips instead).
const TuiBox: React.FC<{
  title?: React.ReactNode;
  bottomTitle?: React.ReactNode;
  children: React.ReactNode;
  style?: React.CSSProperties;
  contentStyle?: React.CSSProperties;
  gradient?: string; // CSS gradient string for the border
}> = ({
  title,
  bottomTitle,
  children,
  style,
  contentStyle,
  gradient = borderGradient(0, 1),
}) => (
  <div
    style={{
      position: "relative",
      background: gradient,
      padding: 2,
      borderRadius: 6,
      display: "flex",
      flexDirection: "column",
      ...style,
    }}
  >
    {title && (
      <div
        style={{
          position: "absolute",
          top: -8,
          left: 10,
          background: COLORS.surface,
          padding: "0 4px",
          fontFamily: FONTS.mono,
          fontSize: 12,
          lineHeight: "16px",
          zIndex: 1,
          whiteSpace: "nowrap",
        }}
      >
        {title}
      </div>
    )}
    {bottomTitle && (
      <div
        style={{
          position: "absolute",
          bottom: -8,
          right: 10,
          background: COLORS.surface,
          padding: "0 4px",
          fontFamily: FONTS.mono,
          fontSize: 12,
          lineHeight: "16px",
          zIndex: 1,
          whiteSpace: "nowrap",
        }}
      >
        {bottomTitle}
      </div>
    )}
    <div
      style={{
        background: COLORS.surface,
        borderRadius: 4,
        flex: 1,
        minHeight: 0,
        overflow: "hidden",
        ...contentStyle,
      }}
    >
      {children}
    </div>
  </div>
);

// The header title for the bird art box, matching the real CLI first border line:
// ╭ actual v0.1.0 ──── https://app.actual.ai ╮
const LogoBoxTitle = (
  <>
    <span style={{ color: COLORS.textPrimary }}>actual </span>
    <span style={{ color: COLORS.borderGreen }}>v0.1.0</span>
    <span style={{ color: COLORS.borderGreen }}> ──── </span>
    <span style={{ color: COLORS.textPrimary }}>https://app.actual.ai</span>
  </>
);

export const TuiLayout: React.FC<TuiLayoutProps> = ({
  steps,
  activeStepIndex,
  outputLines,
  confirmWidget,
  currentFrame,
}) => {
  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        flexGrow: 1,
        background: COLORS.surface,
        color: COLORS.textPrimary,
        // paddingTop: 14px gives top titles room; paddingBottom: 14px gives
        // bottom titles room. The titles are position:absolute at ±8px, so
        // we need >=8px clearance on both ends.
        padding: "14px 8px 14px 8px",
        boxSizing: "border-box",
        overflow: "hidden",
      }}
    >
      {/* Main row: left column + right output box.
          NO overflow:hidden here — TuiBox titles protrude ±8px outside their
          borders via position:absolute. overflow:hidden on this div (which has
          no padding) would clip them. The outer TuiLayout div already has
          overflow:hidden with padding:14px which is the correct clip boundary. */}
      <div
        style={{
          display: "flex",
          flex: 1,
          gap: 8,
          minHeight: 0,
        }}
      >
        {/* Left column: logo box + steps box */}
        <div
          style={{
            width: 330,
            flexShrink: 0,
            display: "flex",
            flexDirection: "column",
            minHeight: 0,
          }}
        >
          {/* Bird art — natural content height (ASCII art + padding).
              Occupies roughly the top 38% of the content area → gradient t=0→0.38 */}
          <TuiBox
            title={LogoBoxTitle}
            style={{ flexShrink: 0 }}
            gradient={borderGradient(0.0, 0.38)}
          >
            <LogoPanel />
          </TuiBox>

          {/* Steps — fills remaining column height.
              Occupies roughly t=0.40→1.0 (after logo + 14px gap) */}
          <TuiBox
            title={<span style={{ color: COLORS.textPrimary }}>Steps</span>}
            style={{ flex: 1, marginTop: 14, minHeight: 0 }}
            contentStyle={{ overflow: "hidden" }}
            gradient={borderGradient(0.40, 1.0)}
          >
            <StepsPanel steps={steps} activeStepIndex={activeStepIndex} currentFrame={currentFrame} />
          </TuiBox>
        </div>

        {/* Right column: output — spans full content height → full gradient t=0→1.0.
            bottomTitle matches real TUI: key hints embedded in Output box bottom border.
            Shows confirm hints when widget active, otherwise standard navigation hints. */}
        <TuiBox
          title={<span style={{ color: COLORS.textPrimary }}>Output</span>}
          gradient={borderGradient(0.0, 1.0)}
          bottomTitle={
            confirmWidget ? (
              <span style={{ color: COLORS.textDim }}>
                ← → select{"  "}Enter confirm
              </span>
            ) : (
              <span style={{ color: COLORS.textDim }}>
                ↑/↓ steps{"  "}q quit
              </span>
            )
          }
          style={{ flex: 1, minHeight: 0 }}
        >
          <OutputPane lines={outputLines} confirmWidget={confirmWidget} currentFrame={currentFrame} />
        </TuiBox>
      </div>
    </div>
  );
};

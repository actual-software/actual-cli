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
}

// Ratatui-style bordered panel. Title (if provided) is overlaid on the top border,
// and bottomTitle (if provided) is overlaid on the bottom border — right-aligned,
// exactly like ratatui's Block::new().title("...").title_bottom("...").
// The outer div is position:relative with NO overflow:hidden so titles at
// top:-8px / bottom:-8px protrude without being clipped.
const TuiBox: React.FC<{
  title?: React.ReactNode;
  bottomTitle?: React.ReactNode;
  children: React.ReactNode;
  style?: React.CSSProperties;
  contentStyle?: React.CSSProperties;
}> = ({ title, bottomTitle, children, style, contentStyle }) => (
  <div
    style={{
      position: "relative",
      border: `1px solid ${COLORS.borderTeal}`,
      borderRadius: 4,
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
    <div style={{ flex: 1, overflow: "hidden", ...contentStyle }}>
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
    <span style={{ color: COLORS.textDim }}> ──── </span>
    <span style={{ color: COLORS.borderTeal }}>https://app.actual.ai</span>
  </>
);

export const TuiLayout: React.FC<TuiLayoutProps> = ({
  steps,
  activeStepIndex,
  outputLines,
  confirmWidget,
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
          {/* Bird art — natural content height (ASCII art + padding) */}
          <TuiBox title={LogoBoxTitle} style={{ flexShrink: 0 }}>
            <LogoPanel />
          </TuiBox>

          {/* Steps — fills remaining column height */}
          <TuiBox
            title={<span style={{ color: COLORS.borderTeal }}>Steps</span>}
            style={{ flex: 1, marginTop: 14, minHeight: 0 }}
            contentStyle={{ overflow: "hidden", flex: "none" }}
          >
            <StepsPanel steps={steps} activeStepIndex={activeStepIndex} />
          </TuiBox>
        </div>

        {/* Right column: output.
            bottomTitle matches real TUI: key hints embedded in Output box bottom border.
            Shows confirm hints when widget active, otherwise standard navigation hints. */}
        <TuiBox
          title={<span style={{ color: COLORS.borderTeal }}>Output</span>}
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
          <OutputPane lines={outputLines} confirmWidget={confirmWidget} />
        </TuiBox>
      </div>
    </div>
  );
};

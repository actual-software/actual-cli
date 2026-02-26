<!-- managed:actual-start -->
<!-- last-synced: 2026-02-26T21:13:35Z -->
<!-- version: 35 -->
<!-- adr-ids: 5e7841bf-40d9-45fc-8012-7aa76bd4100e,58479dc0-55d3-470a-aa47-a240df3ee974,e6da8439-ffbc-45c6-99c6-d5d789d16e06,8cffb459-f7eb-48be-9283-9fda8fc723c5,7049f87a-dd0b-425a-b3fb-db77e1a4f66a,4a3afd2e-282d-478c-af82-abaf9802f508,66744e67-eb55-4847-b77b-3d35c4cdc92f,330add2c-cf7c-41d3-a447-cabe041a4533,10b198a3-89fa-48ad-8401-4ed8c80a780b,8ecd825d-09a9-48b4-87a3-5dd0db6a80ea,eec74a06-99b1-42f5-a34b-0420723200f2,f9a54430-044e-4c06-ab98-78b290519085,6e32ee53-0ef9-43f1-8d37-b3896214ce83 -->

<!-- adr:5e7841bf-40d9-45fc-8012-7aa76bd4100e start -->
## Guard Key Event Handlers with KeyEventKind::Press to Prevent Duplicate Dispatching
- Match only Event::Key events where key.kind == KeyEventKind::Press before dispatching to application logic

- Use a match guard: Event::Key(key) if key.kind == KeyEventKind::Press => { handle_key(key) }
- Import KeyEventKind from crossterm::event
- Handle KeyEventKind::Repeat in a separate match arm only when key-repeat behavior is explicitly desired for that action

<!-- adr:5e7841bf-40d9-45fc-8012-7aa76bd4100e end -->

<!-- adr:58479dc0-55d3-470a-aa47-a240df3ee974 start -->
## Use ratatui::init() for Terminal Setup Instead of Manual Configuration
- Call ratatui::init() to initialize the terminal instead of manually calling enable_raw_mode(), execute!(EnterAlternateScreen), and Terminal::new() in sequence
- Call ratatui::restore() in the cleanup path to match every ratatui::init() call

- Replace manual setup with: let mut terminal = ratatui::init();
- Use ratatui::try_init() when initialization failure must be handled as a Result rather than a panic
- Use ratatui::try_restore() when cleanup errors must be surfaced explicitly
- For non-fullscreen (inline) rendering, use ratatui::init_with_options() with options that skip the alternate screen

<!-- adr:58479dc0-55d3-470a-aa47-a240df3ee974 end -->

<!-- adr:e6da8439-ffbc-45c6-99c6-d5d789d16e06 start -->
## Pin Crossterm to the Version Matching ratatui's Feature Flag to Prevent Duplicate Event Queues
- Select ratatui's crossterm feature flag (e.g. crossterm_0_28) to match the crossterm major version your crate depends on directly
- Pin crossterm explicitly in Cargo.toml to the same major version selected by the feature flag

- Run cargo tree -p crossterm to detect duplicate versions in the dependency graph
- In Cargo.toml: ratatui = { version = "0.30", features = ["crossterm_0_28"] } and crossterm = "0.28"
- For transitive crossterm conflicts, add default-features = false to the dependency pulling the incompatible version

<!-- adr:e6da8439-ffbc-45c6-99c6-d5d789d16e06 end -->

<!-- adr:8cffb459-f7eb-48be-9283-9fda8fc723c5 start -->
## Implement Widget for &App Rather Than the Owned App Type
- Implement Widget for &App, not App, when the render method needs read-only access to application state
- Implement Widget for &mut App only when render requires mutable access to widget state such as ListState

- Write: impl Widget for &App { fn render(self, area: Rect, buf: &mut Buffer) { ... } }
- Call frame.render_widget(self, frame.area()) inside a &mut self draw method — the borrow checker allows this because &App does not move App
- For stateful widgets, hold state (e.g. ListState) as a field on App and pass it via frame.render_stateful_widget(widget, area, &mut self.state)

<!-- adr:8cffb459-f7eb-48be-9283-9fda8fc723c5 end -->

<!-- adr:7049f87a-dd0b-425a-b3fb-db77e1a4f66a start -->
## Render All Widgets Inside a Single terminal.draw() Closure per Event Loop Iteration
- Place every frame.render_widget call for a given frame inside one terminal.draw(|frame| { ... }) closure
- Never call terminal.draw() more than once per main event loop iteration

- Structure the loop body as: terminal.draw(|frame| { render_all(frame); })?;
- Use terminal.try_draw(|frame| -> Result<()> { ... }) (ratatui 0.28+) when the render closure can return an error
- Render popup overlays within the same closure: draw the base UI first, then render Clear followed by the popup widget on top

<!-- adr:7049f87a-dd0b-425a-b3fb-db77e1a4f66a end -->

<!-- adr:4a3afd2e-282d-478c-af82-abaf9802f508 start -->
## Use Text::from_iter() for Multi-Line Content Instead of Newline-Embedded Strings
- Construct multi-line Text values with Text::from_iter(["line 1", "line 2"]) or an explicit Vec<Line>
- Never embed \n in strings passed to Text::from() or Line::from() expecting line splitting

- Replace Text::from("line 1\nline 2") with Text::from_iter(["line 1", "line 2"])
- For styled lines: Text::from(vec![Line::from("line 1"), Line::from("line 2")])
- For mixed-style spans within one line: Line::from_iter([Span::raw("a"), Span::styled("b", style)])
- Audit any pre-0.27 code that relied on newline splitting — it produces incorrect output silently after upgrade

<!-- adr:4a3afd2e-282d-478c-af82-abaf9802f508 end -->

<!-- adr:66744e67-eb55-4847-b77b-3d35c4cdc92f start -->
## Store Stateful Widget State in App and Render with frame.render_stateful_widget()
- Store stateful widget state as a persistent field on the application struct, not as a local variable inside the draw closure
- Render stateful widgets with frame.render_stateful_widget(widget, area, &mut self.state), not frame.render_widget()

- Add state fields to App: list_state: ListState, table_state: TableState
- In the draw closure: frame.render_stateful_widget(List::new(items), area, &mut self.list_state)
- Navigate selection with self.list_state.select_next(), select_previous(), select_first(), select_last() (ratatui 0.27+)
- Initialize ScrollbarState once with ScrollbarState::new(content_length) where content_length is item count, not pixel count

<!-- adr:66744e67-eb55-4847-b77b-3d35c4cdc92f end -->

<!-- adr:330add2c-cf7c-41d3-a447-cabe041a4533 start -->
## Drive the Event Loop with event::poll() and a Tick Duration for Time-Based UI Updates
- Use crossterm::event::poll(timeout) rather than event::read() when the UI requires periodic redraws independent of user input

- Compute a bounded timeout: let timeout = tick_rate.saturating_sub(last_tick.elapsed());
- Poll then read: if event::poll(timeout)? { match event::read()? { ... } }
- After polling, check elapsed time: if last_tick.elapsed() >= tick_rate { handle_tick(); last_tick = Instant::now(); }
- For tokio async runtimes, use crossterm's EventStream (feature event-stream) and await events in a select! loop; keep terminal.draw() on a synchronous path

<!-- adr:330add2c-cf7c-41d3-a447-cabe041a4533 end -->

<!-- adr:10b198a3-89fa-48ad-8401-4ed8c80a780b start -->
## Call color_eyre::install() Before ratatui::init() to Preserve the Panic Hook Chain
- Always call color_eyre::install() before ratatui::init() when both are used in the same application

- In main(): color_eyre::install()?; let mut terminal = ratatui::init();
- color_eyre chains onto the existing hook via std::panic::take_hook() — installing it first ensures ratatui's hook wraps it as the outermost handler
- When using ratatui::run(closure), still call color_eyre::install() first; ratatui::run() calls init() internally

<!-- adr:10b198a3-89fa-48ad-8401-4ed8c80a780b end -->

<!-- adr:8ecd825d-09a9-48b4-87a3-5dd0db6a80ea start -->
## Clamp the Render Area with area.intersection(buf.area) in Custom Widget Implementations
- Reassign area to area.intersection(buf.area) as the first statement in every custom Widget::render implementation

- fn render(self, area: Rect, buf: &mut Buffer) { let area = area.intersection(buf.area); ... }
- Use Rect::clamp(container) to constrain popup or overlay positioning within a parent rect
- Prefer area.rows() and area.columns() iterators over manual coordinate arithmetic to avoid off-by-one buffer writes
- Note: Layout handles intersection automatically for top-level widget composition; the guard is required in leaf widget render methods that perform direct buffer writes

<!-- adr:8ecd825d-09a9-48b4-87a3-5dd0db6a80ea end -->

<!-- adr:eec74a06-99b1-42f5-a34b-0420723200f2 start -->
## Test Widget Rendering with TestBackend and Insta Snapshot Assertions
- Use ratatui::backend::TestBackend as the backend for all widget rendering tests
- Assert full render output with insta's assert_snapshot! rather than hand-written string comparisons

- Construct a fixed-size terminal: Terminal::new(TestBackend::new(80, 20)).unwrap() — width first, height second
- Render: terminal.draw(|frame| frame.render_widget(widget, frame.area())).unwrap();
- Assert: insta::assert_snapshot!(terminal.backend());
- Run cargo insta review to approve or reject snapshot changes interactively
- For buffer-level assertions without insta: assert_eq!(buf, Buffer::with_lines(["row0", "row1"])) — each row string must match the buffer width exactly

<!-- adr:eec74a06-99b1-42f5-a34b-0420723200f2 end -->

<!-- adr:f9a54430-044e-4c06-ab98-78b290519085 start -->
## Write Log Output to a File via tracing-appender, Not to stdout or stderr
- Configure the tracing subscriber to write to a file using tracing-appender rather than to stdout or stderr
- Never use println!, print!, or log macros that target stdout while ratatui is active

- Before ratatui::init(): let file = tracing_appender::rolling::daily("./logs", "app.log"); let (writer, _guard) = tracing_appender::non_blocking(file);
- Bind the guard to a named variable for the full program duration — dropping it immediately closes the log writer
- As an alternative, use the tui-logger crate to render log records as a scrollable widget inside the TUI itself

<!-- adr:f9a54430-044e-4c06-ab98-78b290519085 end -->

<!-- adr:6e32ee53-0ef9-43f1-8d37-b3896214ce83 start -->
## Use frame.area() Instead of the Deprecated frame.size() in Draw Closures
- Call frame.area() to obtain the renderable Rect inside terminal.draw() closures
- Never call frame.size() in code targeting ratatui 0.28+

- Replace all frame.size() calls with frame.area() — the return type and value are identical
- Use terminal.try_draw(|frame| -> Result<()> { ... }) (ratatui 0.28+) when the render closure itself can return an error
- To access the rendered area outside the closure, use the CompletedFrame returned by terminal.draw(): let area = terminal.draw(|f| { ... })?.area;

<!-- adr:6e32ee53-0ef9-43f1-8d37-b3896214ce83 end -->

<!-- managed:actual-end -->

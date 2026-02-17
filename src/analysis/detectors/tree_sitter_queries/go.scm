; Go tree-sitter queries for architecture facet extraction

; =============================================================================
; PUBLIC API - Exported functions and types (leaf_id: 25)
; =============================================================================

; @rule_id: ts.go.exported_function
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.9
(function_declaration
  name: (identifier) @fn_name
  (#match? @fn_name "^[A-Z]"))

; @rule_id: ts.go.exported_method
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.9
(method_declaration
  name: (field_identifier) @method_name
  (#match? @method_name "^[A-Z]"))

; @rule_id: ts.go.exported_type
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.9
(type_declaration
  (type_spec
    name: (type_identifier) @type_name
    (#match? @type_name "^[A-Z]")))

; @rule_id: ts.go.exported_interface
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.95
(type_declaration
  (type_spec
    name: (type_identifier) @type_name
    type: (interface_type))
  (#match? @type_name "^[A-Z]"))

; @rule_id: ts.go.exported_struct
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.9
(type_declaration
  (type_spec
    name: (type_identifier) @type_name
    type: (struct_type))
  (#match? @type_name "^[A-Z]"))

; =============================================================================
; IMPORTS - Dependencies (leaf_id: 20)
; =============================================================================

; @rule_id: ts.go.import_spec
; @facet_slot: libs.core.detected
; @leaf_id: 20
; @confidence: 0.9
(import_spec
  path: (interpreted_string_literal) @import_path)

; =============================================================================
; FRAMEWORK PATTERNS - HTTP handlers (leaf_id: 23)
; =============================================================================

; @rule_id: ts.go.http_handler
; @facet_slot: boundaries.service_definitions
; @leaf_id: 23
; @confidence: 0.85
(function_declaration
  name: (identifier) @fn_name
  parameters: (parameter_list
    (parameter_declaration
      type: (qualified_type
        package: (package_identifier) @pkg (#eq? @pkg "http")
        name: (type_identifier) @type (#eq? @type "ResponseWriter")))
    (parameter_declaration
      type: (pointer_type
        (qualified_type
          package: (package_identifier) @pkg2 (#eq? @pkg2 "http")
          name: (type_identifier) @type2 (#eq? @type2 "Request"))))))

; @rule_id: ts.go.gin_handler
; @facet_slot: boundaries.service_definitions
; @leaf_id: 23
; @confidence: 0.9
(call_expression
  function: (selector_expression
    operand: (identifier) @obj
    field: (field_identifier) @method)
  (#match? @method "^(GET|POST|PUT|DELETE|PATCH|OPTIONS|HEAD|Any|Handle)$"))

; @rule_id: ts.go.echo_handler
; @facet_slot: boundaries.service_definitions
; @leaf_id: 23
; @confidence: 0.9
(call_expression
  function: (selector_expression
    operand: (identifier) @obj
    field: (field_identifier) @method)
  (#match? @method "^(GET|POST|PUT|DELETE|PATCH|OPTIONS|HEAD|Any|Add)$"))

; =============================================================================
; CONCURRENCY PATTERNS (leaf_id: 17)
; =============================================================================

; @rule_id: ts.go.goroutine
; @facet_slot: paradigm.concurrency_model
; @leaf_id: 17
; @confidence: 0.95
(go_statement
  (call_expression) @goroutine_call)

; @rule_id: ts.go.channel_operation
; @facet_slot: paradigm.concurrency_model
; @leaf_id: 17
; @confidence: 0.9
(send_statement
  channel: (_) @channel)

; @rule_id: ts.go.select_statement
; @facet_slot: paradigm.concurrency_model
; @leaf_id: 17
; @confidence: 0.9
(select_statement) @select_block

; =============================================================================
; CONFIGURATION (leaf_id: 37)
; =============================================================================

; @rule_id: ts.go.os_getenv
; @facet_slot: runtime.config.sources
; @leaf_id: 37
; @confidence: 0.9
(call_expression
  function: (selector_expression
    operand: (identifier) @pkg (#eq? @pkg "os")
    field: (field_identifier) @method (#eq? @method "Getenv"))
  arguments: (argument_list
    (interpreted_string_literal) @env_var))

; @rule_id: ts.go.viper
; @facet_slot: runtime.config.sources
; @leaf_id: 37
; @confidence: 0.9
(call_expression
  function: (selector_expression
    operand: (identifier) @pkg (#eq? @pkg "viper")
    field: (field_identifier) @method))

; =============================================================================
; DATABASE CLIENTS (leaf_id: 28)
; =============================================================================

; @rule_id: ts.go.sql_open
; @facet_slot: data.primary_datastores
; @leaf_id: 28
; @confidence: 0.9
(call_expression
  function: (selector_expression
    operand: (identifier) @pkg (#eq? @pkg "sql")
    field: (field_identifier) @method (#eq? @method "Open")))

; @rule_id: ts.go.gorm
; @facet_slot: data.access.patterns
; @leaf_id: 30
; @confidence: 0.9
(call_expression
  function: (selector_expression
    operand: (identifier) @pkg (#eq? @pkg "gorm")
    field: (field_identifier) @method (#eq? @method "Open")))

; =============================================================================
; OBSERVABILITY - Logging (leaf_id: 46)
; =============================================================================

; @rule_id: ts.go.zap_logger
; @facet_slot: obs.logging
; @leaf_id: 46
; @confidence: 0.9
(call_expression
  function: (selector_expression
    operand: (identifier) @pkg (#eq? @pkg "zap")
    field: (field_identifier) @method))

; @rule_id: ts.go.logrus
; @facet_slot: obs.logging
; @leaf_id: 46
; @confidence: 0.9
(call_expression
  function: (selector_expression
    operand: (identifier) @pkg (#eq? @pkg "logrus")
    field: (field_identifier) @method))

; =============================================================================
; TESTING (leaf_id: 39)
; =============================================================================

; @rule_id: ts.go.test_function
; @facet_slot: test.frameworks
; @leaf_id: 39
; @confidence: 0.95
(function_declaration
  name: (identifier) @fn_name
  (#match? @fn_name "^Test")
  parameters: (parameter_list
    (parameter_declaration
      type: (pointer_type
        (qualified_type
          package: (package_identifier) @pkg (#eq? @pkg "testing")
          name: (type_identifier) @type (#eq? @type "T"))))))

; @rule_id: ts.go.benchmark_function
; @facet_slot: test.frameworks
; @leaf_id: 39
; @confidence: 0.9
(function_declaration
  name: (identifier) @fn_name
  (#match? @fn_name "^Benchmark"))

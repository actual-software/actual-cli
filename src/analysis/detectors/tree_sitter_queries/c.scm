; C tree-sitter queries for architecture facet extraction

; =============================================================================
; PUBLIC API - Functions and types (leaf_id: 25)
; =============================================================================

; @rule_id: ts.c.function_definition
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.8
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @fn_name))

; @rule_id: ts.c.function_declaration
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.85
(declaration
  declarator: (function_declarator
    declarator: (identifier) @fn_name))

; @rule_id: ts.c.struct_definition
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.8
(struct_specifier
  name: (type_identifier) @struct_name
  body: (field_declaration_list))

; @rule_id: ts.c.typedef
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.85
(type_definition
  declarator: (type_identifier) @type_name)

; @rule_id: ts.c.enum_definition
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.8
(enum_specifier
  name: (type_identifier) @enum_name
  body: (enumerator_list))

; =============================================================================
; INCLUDES - Dependencies (leaf_id: 20)
; =============================================================================

; @rule_id: ts.c.include_system
; @facet_slot: libs.core.detected
; @leaf_id: 20
; @confidence: 0.9
(preproc_include
  path: (system_lib_string) @include_path)

; @rule_id: ts.c.include_local
; @facet_slot: libs.core.detected
; @leaf_id: 20
; @confidence: 0.85
(preproc_include
  path: (string_literal) @include_path)

; =============================================================================
; PREPROCESSOR - Build configuration (leaf_id: 49)
; =============================================================================

; @rule_id: ts.c.define
; @facet_slot: runtime.config.sources
; @leaf_id: 37
; @confidence: 0.7
(preproc_def
  name: (identifier) @macro_name)

; @rule_id: ts.c.ifdef
; @facet_slot: runtime.config.sources
; @leaf_id: 37
; @confidence: 0.7
(preproc_ifdef
  name: (identifier) @condition)

; =============================================================================
; MEMORY PATTERNS (leaf_id: 38)
; =============================================================================

; @rule_id: ts.c.malloc_call
; @facet_slot: runtime.resource_controls
; @leaf_id: 38
; @confidence: 0.85
(call_expression
  function: (identifier) @fn (#match? @fn "^(malloc|calloc|realloc|free)$"))

; =============================================================================
; CONCURRENCY (leaf_id: 17)
; =============================================================================

; @rule_id: ts.c.pthread
; @facet_slot: paradigm.concurrency_model
; @leaf_id: 17
; @confidence: 0.9
(call_expression
  function: (identifier) @fn (#match? @fn "^pthread_"))

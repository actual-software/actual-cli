; C++ tree-sitter queries for architecture facet extraction

; =============================================================================
; PUBLIC API - Classes and functions (leaf_id: 25)
; =============================================================================

; @rule_id: ts.cpp.class_definition
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.9
(class_specifier
  name: (type_identifier) @class_name
  body: (field_declaration_list))

; @rule_id: ts.cpp.struct_definition
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.85
(struct_specifier
  name: (type_identifier) @struct_name
  body: (field_declaration_list))

; @rule_id: ts.cpp.function_definition
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.8
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @fn_name))

; @rule_id: ts.cpp.method_definition
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.85
(function_definition
  declarator: (function_declarator
    declarator: (qualified_identifier
      name: (identifier) @method_name)))

; @rule_id: ts.cpp.template_class
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.9
(template_declaration
  (class_specifier
    name: (type_identifier) @class_name))

; @rule_id: ts.cpp.namespace
; @facet_slot: structure.layering
; @leaf_id: 24
; @confidence: 0.8
(namespace_definition
  name: (identifier) @namespace_name)

; =============================================================================
; INCLUDES - Dependencies (leaf_id: 20)
; =============================================================================

; @rule_id: ts.cpp.include_system
; @facet_slot: libs.core.detected
; @leaf_id: 20
; @confidence: 0.9
(preproc_include
  path: (system_lib_string) @include_path)

; @rule_id: ts.cpp.include_local
; @facet_slot: libs.core.detected
; @leaf_id: 20
; @confidence: 0.85
(preproc_include
  path: (string_literal) @include_path)

; =============================================================================
; INHERITANCE AND INTERFACES (leaf_id: 22)
; =============================================================================

; @rule_id: ts.cpp.class_inheritance
; @facet_slot: arch.style
; @leaf_id: 22
; @confidence: 0.85
(class_specifier
  name: (type_identifier) @class_name
  (base_class_clause
    (type_identifier) @base_class))

; @rule_id: ts.cpp.virtual_method
; @facet_slot: api.public.protocols
; @leaf_id: 25
; @confidence: 0.9
(function_definition
  (virtual_specifier) @virtual
  declarator: (function_declarator
    declarator: (identifier) @method_name))

; =============================================================================
; SMART POINTERS / MEMORY (leaf_id: 38)
; =============================================================================

; @rule_id: ts.cpp.smart_pointer
; @facet_slot: runtime.resource_controls
; @leaf_id: 38
; @confidence: 0.9
(template_type
  name: (type_identifier) @ptr_type
  (#match? @ptr_type "^(unique_ptr|shared_ptr|weak_ptr)$"))

; @rule_id: ts.cpp.new_expression
; @facet_slot: runtime.resource_controls
; @leaf_id: 38
; @confidence: 0.8
(new_expression
  type: (_) @allocated_type)

; =============================================================================
; CONCURRENCY (leaf_id: 17)
; =============================================================================

; @rule_id: ts.cpp.std_thread
; @facet_slot: paradigm.concurrency_model
; @leaf_id: 17
; @confidence: 0.9
(qualified_identifier
  scope: (namespace_identifier) @ns (#eq? @ns "std")
  name: (identifier) @type (#eq? @type "thread"))

; @rule_id: ts.cpp.std_mutex
; @facet_slot: paradigm.concurrency_model
; @leaf_id: 17
; @confidence: 0.9
(qualified_identifier
  scope: (namespace_identifier) @ns (#eq? @ns "std")
  name: (identifier) @type (#match? @type "^(mutex|lock_guard|unique_lock)$"))

; @rule_id: ts.cpp.std_async
; @facet_slot: paradigm.concurrency_model
; @leaf_id: 17
; @confidence: 0.9
(call_expression
  function: (qualified_identifier
    scope: (namespace_identifier) @ns (#eq? @ns "std")
    name: (identifier) @fn (#eq? @fn "async")))

; =============================================================================
; TESTING (leaf_id: 39)
; =============================================================================

; @rule_id: ts.cpp.gtest
; @facet_slot: test.frameworks
; @leaf_id: 39
; @confidence: 0.95
(call_expression
  function: (identifier) @fn (#match? @fn "^(TEST|TEST_F|TEST_P|EXPECT_|ASSERT_)"))

; @rule_id: ts.cpp.catch2
; @facet_slot: test.frameworks
; @leaf_id: 39
; @confidence: 0.9
(call_expression
  function: (identifier) @fn (#match? @fn "^(TEST_CASE|SECTION|REQUIRE|CHECK)"))

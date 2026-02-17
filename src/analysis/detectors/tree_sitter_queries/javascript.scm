; JavaScript tree-sitter queries for architecture facet extraction
; Each query has metadata comments that define the mapping to facets

; =============================================================================
; PUBLIC API - Exports (leaf_id: 25)
; =============================================================================

; @rule_id: ts.js.export_default
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.9
(export_statement
  "default"
  (function_declaration
    name: (identifier) @export_name))

; @rule_id: ts.js.export_default_class
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.9
(export_statement
  "default"
  (class_declaration
    name: (identifier) @export_name))

; @rule_id: ts.js.named_export
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.85
(export_statement
  (lexical_declaration
    (variable_declarator
      name: (identifier) @export_name)))

; @rule_id: ts.js.export_function
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.9
(export_statement
  (function_declaration
    name: (identifier) @export_name))

; @rule_id: ts.js.export_class
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.9
(export_statement
  (class_declaration
    name: (identifier) @export_name))

; @rule_id: ts.js.module_exports
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.8
(assignment_expression
  left: (member_expression
    object: (identifier) @obj (#eq? @obj "module")
    property: (property_identifier) @prop (#eq? @prop "exports"))
  right: (_) @export_value)

; =============================================================================
; IMPORTS - Dependencies (leaf_id: 20)
; =============================================================================

; @rule_id: ts.js.import_declaration
; @facet_slot: libs.core.detected
; @leaf_id: 20
; @confidence: 0.9
(import_statement
  source: (string) @import_source)

; @rule_id: ts.js.require_call
; @facet_slot: libs.core.detected
; @leaf_id: 20
; @confidence: 0.85
(call_expression
  function: (identifier) @fn (#eq? @fn "require")
  arguments: (arguments
    (string) @import_source))

; =============================================================================
; FRAMEWORK PATTERNS - Express/Fastify routes (leaf_id: 23)
; =============================================================================

; @rule_id: ts.js.express_route
; @facet_slot: boundaries.service_definitions
; @leaf_id: 23
; @confidence: 0.9
(call_expression
  function: (member_expression
    object: (identifier) @obj
    property: (property_identifier) @method)
  (#match? @method "^(get|post|put|delete|patch|options|head|all|use)$")
  arguments: (arguments
    (string) @route_path))

; @rule_id: ts.js.router_route
; @facet_slot: boundaries.service_definitions
; @leaf_id: 23
; @confidence: 0.85
(call_expression
  function: (member_expression
    object: (identifier) @obj (#eq? @obj "router")
    property: (property_identifier) @method)
  (#match? @method "^(get|post|put|delete|patch|options|head|all|use)$"))

; =============================================================================
; ASYNC/CONCURRENCY PATTERNS (leaf_id: 17)
; =============================================================================

; @rule_id: ts.js.async_function
; @facet_slot: paradigm.concurrency_model
; @leaf_id: 17
; @confidence: 0.9
(function_declaration
  "async"
  name: (identifier) @fn_name)

; @rule_id: ts.js.arrow_async
; @facet_slot: paradigm.concurrency_model
; @leaf_id: 17
; @confidence: 0.85
(arrow_function
  "async")

; =============================================================================
; CONFIGURATION - Environment variables (leaf_id: 37)
; =============================================================================

; @rule_id: ts.js.process_env
; @facet_slot: runtime.config.sources
; @leaf_id: 37
; @confidence: 0.9
(member_expression
  object: (member_expression
    object: (identifier) @obj (#eq? @obj "process")
    property: (property_identifier) @prop (#eq? @prop "env"))
  property: (property_identifier) @env_var)

; =============================================================================
; DATABASE CLIENTS (leaf_id: 28)
; =============================================================================

; @rule_id: ts.js.prisma_client
; @facet_slot: data.primary_datastores
; @leaf_id: 28
; @confidence: 0.9
(new_expression
  constructor: (identifier) @cls (#eq? @cls "PrismaClient"))

; @rule_id: ts.js.mongoose_model
; @facet_slot: data.primary_datastores
; @leaf_id: 28
; @confidence: 0.85
(call_expression
  function: (member_expression
    object: (identifier) @obj (#eq? @obj "mongoose")
    property: (property_identifier) @method (#eq? @method "model")))

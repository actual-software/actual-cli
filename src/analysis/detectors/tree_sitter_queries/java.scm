; Java tree-sitter queries for architecture facet extraction

; =============================================================================
; PUBLIC API - Classes, Interfaces, Methods (leaf_id: 25)
; =============================================================================

; @rule_id: ts.java.public_class
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.9
(class_declaration
  (modifiers "public")
  name: (identifier) @class_name)

; @rule_id: ts.java.public_interface
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.95
(interface_declaration
  (modifiers "public")
  name: (identifier) @interface_name)

; @rule_id: ts.java.public_method
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.85
(method_declaration
  (modifiers "public")
  name: (identifier) @method_name)

; @rule_id: ts.java.public_enum
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.85
(enum_declaration
  (modifiers "public")
  name: (identifier) @enum_name)

; =============================================================================
; IMPORTS - Dependencies (leaf_id: 20)
; =============================================================================

; @rule_id: ts.java.import_declaration
; @facet_slot: libs.core.detected
; @leaf_id: 20
; @confidence: 0.9
(import_declaration
  (scoped_identifier) @import_path)

; =============================================================================
; FRAMEWORK PATTERNS - Spring (leaf_id: 19)
; =============================================================================

; @rule_id: ts.java.spring_controller
; @facet_slot: framework.app.detected
; @leaf_id: 19
; @confidence: 0.95
(class_declaration
  (modifiers
    (marker_annotation
      name: (identifier) @ann (#match? @ann "^(Controller|RestController)$"))))

; @rule_id: ts.java.spring_service
; @facet_slot: framework.di.detected
; @leaf_id: 19
; @confidence: 0.95
(class_declaration
  (modifiers
    (marker_annotation
      name: (identifier) @ann (#eq? @ann "Service"))))

; @rule_id: ts.java.spring_component
; @facet_slot: framework.di.detected
; @leaf_id: 19
; @confidence: 0.9
(class_declaration
  (modifiers
    (marker_annotation
      name: (identifier) @ann (#eq? @ann "Component"))))

; @rule_id: ts.java.spring_repository
; @facet_slot: data.access.patterns
; @leaf_id: 30
; @confidence: 0.95
(class_declaration
  (modifiers
    (marker_annotation
      name: (identifier) @ann (#eq? @ann "Repository"))))

; @rule_id: ts.java.spring_configuration
; @facet_slot: runtime.config.sources
; @leaf_id: 37
; @confidence: 0.9
(class_declaration
  (modifiers
    (marker_annotation
      name: (identifier) @ann (#eq? @ann "Configuration"))))

; =============================================================================
; SERVICE BOUNDARIES - HTTP endpoints (leaf_id: 23)
; =============================================================================

; @rule_id: ts.java.request_mapping
; @facet_slot: boundaries.service_definitions
; @leaf_id: 23
; @confidence: 0.95
(method_declaration
  (modifiers
    (annotation
      name: (identifier) @ann (#match? @ann "^(GetMapping|PostMapping|PutMapping|DeleteMapping|PatchMapping|RequestMapping)$"))))

; @rule_id: ts.java.path_annotation
; @facet_slot: boundaries.service_definitions
; @leaf_id: 23
; @confidence: 0.9
(method_declaration
  (modifiers
    (annotation
      name: (identifier) @ann (#eq? @ann "Path"))))

; =============================================================================
; DEPENDENCY INJECTION (leaf_id: 19)
; =============================================================================

; @rule_id: ts.java.autowired
; @facet_slot: framework.di.detected
; @leaf_id: 19
; @confidence: 0.95
(field_declaration
  (modifiers
    (marker_annotation
      name: (identifier) @ann (#eq? @ann "Autowired"))))

; @rule_id: ts.java.inject
; @facet_slot: framework.di.detected
; @leaf_id: 19
; @confidence: 0.95
(field_declaration
  (modifiers
    (marker_annotation
      name: (identifier) @ann (#eq? @ann "Inject"))))

; =============================================================================
; DATA ACCESS - JPA/Hibernate (leaf_id: 28, 30)
; =============================================================================

; @rule_id: ts.java.jpa_entity
; @facet_slot: data.access.patterns
; @leaf_id: 30
; @confidence: 0.95
(class_declaration
  (modifiers
    (marker_annotation
      name: (identifier) @ann (#eq? @ann "Entity"))))

; @rule_id: ts.java.jpa_table
; @facet_slot: data.primary_datastores
; @leaf_id: 28
; @confidence: 0.9
(class_declaration
  (modifiers
    (annotation
      name: (identifier) @ann (#eq? @ann "Table"))))

; @rule_id: ts.java.jpa_query
; @facet_slot: data.access.patterns
; @leaf_id: 30
; @confidence: 0.9
(method_declaration
  (modifiers
    (annotation
      name: (identifier) @ann (#eq? @ann "Query"))))

; =============================================================================
; AUTHENTICATION/AUTHORIZATION (leaf_id: 32, 33)
; =============================================================================

; @rule_id: ts.java.spring_security
; @facet_slot: authn.methods
; @leaf_id: 32
; @confidence: 0.9
(method_declaration
  (modifiers
    (annotation
      name: (identifier) @ann (#match? @ann "^(PreAuthorize|Secured|RolesAllowed)$"))))

; =============================================================================
; OBSERVABILITY - Logging (leaf_id: 46)
; =============================================================================

; @rule_id: ts.java.slf4j_logger
; @facet_slot: obs.logging
; @leaf_id: 46
; @confidence: 0.95
(field_declaration
  type: (type_identifier) @type (#eq? @type "Logger")
  declarator: (variable_declarator
    name: (identifier) @logger_name))

; @rule_id: ts.java.lombok_slf4j
; @facet_slot: obs.logging
; @leaf_id: 46
; @confidence: 0.95
(class_declaration
  (modifiers
    (marker_annotation
      name: (identifier) @ann (#eq? @ann "Slf4j"))))

; =============================================================================
; TESTING (leaf_id: 39)
; =============================================================================

; @rule_id: ts.java.junit_test
; @facet_slot: test.frameworks
; @leaf_id: 39
; @confidence: 0.95
(method_declaration
  (modifiers
    (marker_annotation
      name: (identifier) @ann (#eq? @ann "Test"))))

; @rule_id: ts.java.test_class
; @facet_slot: test.frameworks
; @leaf_id: 39
; @confidence: 0.85
(class_declaration
  name: (identifier) @class_name
  (#match? @class_name "Test$"))

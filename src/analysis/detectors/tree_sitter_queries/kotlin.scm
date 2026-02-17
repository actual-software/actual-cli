; Kotlin tree-sitter queries for architecture facet extraction

; =============================================================================
; PUBLIC API - Classes, Interfaces, Functions (leaf_id: 25)
; =============================================================================

; @rule_id: ts.kotlin.public_class
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.9
(class_declaration
  (modifiers
    (visibility_modifier) @mod (#eq? @mod "public"))
  (type_identifier) @class_name)

; @rule_id: ts.kotlin.open_class
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.85
(class_declaration
  (modifiers
    (inheritance_modifier) @mod (#eq? @mod "open"))
  (type_identifier) @class_name)

; @rule_id: ts.kotlin.data_class
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.8
(class_declaration
  (modifiers
    (class_modifier) @mod (#eq? @mod "data"))
  (type_identifier) @class_name)

; @rule_id: ts.kotlin.interface
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.95
(class_declaration
  "interface"
  (type_identifier) @interface_name)

; @rule_id: ts.kotlin.public_function
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.85
(function_declaration
  (modifiers
    (visibility_modifier) @mod (#eq? @mod "public"))
  (simple_identifier) @fn_name)

; @rule_id: ts.kotlin.object_declaration
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.85
(object_declaration
  (type_identifier) @object_name)

; @rule_id: ts.kotlin.sealed_class
; @facet_slot: api.public.contracts
; @leaf_id: 25
; @confidence: 0.9
(class_declaration
  (modifiers
    (class_modifier) @mod (#eq? @mod "sealed"))
  (type_identifier) @class_name)

; =============================================================================
; IMPORTS - Dependencies (leaf_id: 20)
; =============================================================================

; @rule_id: ts.kotlin.import_declaration
; @facet_slot: libs.core.detected
; @leaf_id: 20
; @confidence: 0.9
(import_header
  (identifier) @import_path)

; =============================================================================
; FRAMEWORK PATTERNS - Spring/Ktor (leaf_id: 19)
; =============================================================================

; @rule_id: ts.kotlin.spring_controller
; @facet_slot: framework.app.detected
; @leaf_id: 19
; @confidence: 0.95
(class_declaration
  (modifiers
    (annotation
      (user_type
        (type_identifier) @ann (#match? @ann "^(Controller|RestController)$")))))

; @rule_id: ts.kotlin.spring_service
; @facet_slot: framework.di.detected
; @leaf_id: 19
; @confidence: 0.95
(class_declaration
  (modifiers
    (annotation
      (user_type
        (type_identifier) @ann (#eq? @ann "Service")))))

; @rule_id: ts.kotlin.spring_component
; @facet_slot: framework.di.detected
; @leaf_id: 19
; @confidence: 0.9
(class_declaration
  (modifiers
    (annotation
      (user_type
        (type_identifier) @ann (#eq? @ann "Component")))))

; @rule_id: ts.kotlin.spring_repository
; @facet_slot: data.access.patterns
; @leaf_id: 30
; @confidence: 0.95
(class_declaration
  (modifiers
    (annotation
      (user_type
        (type_identifier) @ann (#eq? @ann "Repository")))))

; @rule_id: ts.kotlin.spring_configuration
; @facet_slot: runtime.config.sources
; @leaf_id: 37
; @confidence: 0.9
(class_declaration
  (modifiers
    (annotation
      (user_type
        (type_identifier) @ann (#eq? @ann "Configuration")))))

; =============================================================================
; SERVICE BOUNDARIES - HTTP endpoints (leaf_id: 23)
; =============================================================================

; @rule_id: ts.kotlin.request_mapping
; @facet_slot: boundaries.service_definitions
; @leaf_id: 23
; @confidence: 0.95
(function_declaration
  (modifiers
    (annotation
      (user_type
        (type_identifier) @ann (#match? @ann "^(GetMapping|PostMapping|PutMapping|DeleteMapping|PatchMapping|RequestMapping)$")))))

; @rule_id: ts.kotlin.ktor_route
; @facet_slot: boundaries.service_definitions
; @leaf_id: 23
; @confidence: 0.9
(call_expression
  (simple_identifier) @fn (#match? @fn "^(get|post|put|delete|patch|route)$"))

; =============================================================================
; DEPENDENCY INJECTION (leaf_id: 19)
; =============================================================================

; @rule_id: ts.kotlin.autowired
; @facet_slot: framework.di.detected
; @leaf_id: 19
; @confidence: 0.95
(property_declaration
  (modifiers
    (annotation
      (user_type
        (type_identifier) @ann (#eq? @ann "Autowired")))))

; @rule_id: ts.kotlin.inject
; @facet_slot: framework.di.detected
; @leaf_id: 19
; @confidence: 0.95
(property_declaration
  (modifiers
    (annotation
      (user_type
        (type_identifier) @ann (#eq? @ann "Inject")))))

; @rule_id: ts.kotlin.koin_inject
; @facet_slot: framework.di.detected
; @leaf_id: 19
; @confidence: 0.9
(call_expression
  (simple_identifier) @fn (#match? @fn "^(inject|get|koinInject)$"))

; =============================================================================
; DATA ACCESS - JPA/Exposed (leaf_id: 28, 30)
; =============================================================================

; @rule_id: ts.kotlin.jpa_entity
; @facet_slot: data.access.patterns
; @leaf_id: 30
; @confidence: 0.95
(class_declaration
  (modifiers
    (annotation
      (user_type
        (type_identifier) @ann (#eq? @ann "Entity")))))

; @rule_id: ts.kotlin.jpa_table
; @facet_slot: data.primary_datastores
; @leaf_id: 28
; @confidence: 0.9
(class_declaration
  (modifiers
    (annotation
      (user_type
        (type_identifier) @ann (#eq? @ann "Table")))))

; @rule_id: ts.kotlin.exposed_table
; @facet_slot: data.access.patterns
; @leaf_id: 30
; @confidence: 0.9
(object_declaration
  (delegation_specifier
    (user_type
      (type_identifier) @type (#match? @type "^(Table|IntIdTable|LongIdTable|UUIDTable)$"))))

; =============================================================================
; COROUTINES - Async patterns (leaf_id: 24)
; =============================================================================

; @rule_id: ts.kotlin.suspend_function
; @facet_slot: async.patterns
; @leaf_id: 24
; @confidence: 0.9
(function_declaration
  (modifiers
    (function_modifier) @mod (#eq? @mod "suspend"))
  (simple_identifier) @fn_name)

; @rule_id: ts.kotlin.coroutine_scope
; @facet_slot: async.patterns
; @leaf_id: 24
; @confidence: 0.85
(call_expression
  (simple_identifier) @fn (#match? @fn "^(launch|async|runBlocking|withContext|coroutineScope)$"))

; @rule_id: ts.kotlin.flow
; @facet_slot: async.patterns
; @leaf_id: 24
; @confidence: 0.9
(call_expression
  (simple_identifier) @fn (#match? @fn "^(flow|flowOf|channelFlow|callbackFlow)$"))

; =============================================================================
; AUTHENTICATION/AUTHORIZATION (leaf_id: 32, 33)
; =============================================================================

; @rule_id: ts.kotlin.spring_security
; @facet_slot: authn.methods
; @leaf_id: 32
; @confidence: 0.9
(function_declaration
  (modifiers
    (annotation
      (user_type
        (type_identifier) @ann (#match? @ann "^(PreAuthorize|Secured|RolesAllowed)$")))))

; =============================================================================
; SERIALIZATION (leaf_id: 26)
; =============================================================================

; @rule_id: ts.kotlin.serializable
; @facet_slot: api.serialization
; @leaf_id: 26
; @confidence: 0.9
(class_declaration
  (modifiers
    (annotation
      (user_type
        (type_identifier) @ann (#eq? @ann "Serializable")))))

; @rule_id: ts.kotlin.json_property
; @facet_slot: api.serialization
; @leaf_id: 26
; @confidence: 0.85
(property_declaration
  (modifiers
    (annotation
      (user_type
        (type_identifier) @ann (#match? @ann "^(JsonProperty|SerialName)$")))))

; =============================================================================
; OBSERVABILITY - Logging (leaf_id: 46)
; =============================================================================

; @rule_id: ts.kotlin.logger_property
; @facet_slot: obs.logging
; @leaf_id: 46
; @confidence: 0.9
(property_declaration
  (variable_declaration
    (simple_identifier) @prop_name (#match? @prop_name "^(logger|log|LOG)$")))

; @rule_id: ts.kotlin.kotlin_logging
; @facet_slot: obs.logging
; @leaf_id: 46
; @confidence: 0.95
(call_expression
  (call_expression
    (simple_identifier) @fn (#eq? @fn "KotlinLogging"))
  (call_suffix
    (value_arguments
      (value_argument))))

; =============================================================================
; TESTING (leaf_id: 39)
; =============================================================================

; @rule_id: ts.kotlin.junit_test
; @facet_slot: test.frameworks
; @leaf_id: 39
; @confidence: 0.95
(function_declaration
  (modifiers
    (annotation
      (user_type
        (type_identifier) @ann (#eq? @ann "Test")))))

; @rule_id: ts.kotlin.kotest_spec
; @facet_slot: test.frameworks
; @leaf_id: 39
; @confidence: 0.9
(class_declaration
  (delegation_specifier
    (user_type
      (type_identifier) @type (#match? @type "^(StringSpec|FunSpec|BehaviorSpec|DescribeSpec|WordSpec|FreeSpec|FeatureSpec|ShouldSpec)$"))))

; @rule_id: ts.kotlin.test_class
; @facet_slot: test.frameworks
; @leaf_id: 39
; @confidence: 0.85
(class_declaration
  (type_identifier) @class_name
  (#match? @class_name "Test$"))

; =============================================================================
; ANDROID SPECIFIC (leaf_id: 19)
; =============================================================================

; @rule_id: ts.kotlin.android_activity
; @facet_slot: framework.app.detected
; @leaf_id: 19
; @confidence: 0.95
(class_declaration
  (delegation_specifier
    (user_type
      (type_identifier) @type (#match? @type "^(Activity|AppCompatActivity|ComponentActivity|FragmentActivity)$"))))

; @rule_id: ts.kotlin.android_fragment
; @facet_slot: framework.app.detected
; @leaf_id: 19
; @confidence: 0.95
(class_declaration
  (delegation_specifier
    (user_type
      (type_identifier) @type (#eq? @type "Fragment"))))

; @rule_id: ts.kotlin.android_viewmodel
; @facet_slot: framework.app.detected
; @leaf_id: 19
; @confidence: 0.9
(class_declaration
  (delegation_specifier
    (user_type
      (type_identifier) @type (#match? @type "^(ViewModel|AndroidViewModel)$"))))

; @rule_id: ts.kotlin.compose_composable
; @facet_slot: framework.app.detected
; @leaf_id: 19
; @confidence: 0.95
(function_declaration
  (modifiers
    (annotation
      (user_type
        (type_identifier) @ann (#eq? @ann "Composable")))))

; @rule_id: ts.kotlin.hilt_inject
; @facet_slot: framework.di.detected
; @leaf_id: 19
; @confidence: 0.95
(class_declaration
  (modifiers
    (annotation
      (user_type
        (type_identifier) @ann (#match? @ann "^(HiltViewModel|AndroidEntryPoint|Inject)$")))))

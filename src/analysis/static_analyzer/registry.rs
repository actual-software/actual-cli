/// A static mapping from a dependency name to framework metadata.
pub struct FrameworkSignature {
    pub dependency: &'static str,
    pub framework_name: &'static str,
    pub category: &'static str,
}

/// Registry of known frameworks identified by their dependency/package name.
///
/// Each entry maps a dependency string (as it appears in a manifest file) to
/// a human-readable framework name and a category string that can be parsed
/// into [`crate::analysis::types::FrameworkCategory`].
pub const FRAMEWORK_REGISTRY: &[FrameworkSignature] = &[
    // ── JS/TS ────────────────────────────────────────────────────────
    FrameworkSignature {
        dependency: "react",
        framework_name: "react",
        category: "web-frontend",
    },
    FrameworkSignature {
        dependency: "next",
        framework_name: "nextjs",
        category: "web-frontend",
    },
    FrameworkSignature {
        dependency: "vue",
        framework_name: "vue",
        category: "web-frontend",
    },
    FrameworkSignature {
        dependency: "nuxt",
        framework_name: "nuxt",
        category: "web-frontend",
    },
    FrameworkSignature {
        dependency: "@angular/core",
        framework_name: "angular",
        category: "web-frontend",
    },
    FrameworkSignature {
        dependency: "svelte",
        framework_name: "svelte",
        category: "web-frontend",
    },
    FrameworkSignature {
        dependency: "express",
        framework_name: "express",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "fastify",
        framework_name: "fastify",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "@nestjs/core",
        framework_name: "nestjs",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "hono",
        framework_name: "hono",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "koa",
        framework_name: "koa",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "@remix-run/node",
        framework_name: "remix",
        category: "web-frontend",
    },
    FrameworkSignature {
        dependency: "astro",
        framework_name: "astro",
        category: "web-frontend",
    },
    FrameworkSignature {
        dependency: "vite",
        framework_name: "vite",
        category: "build-system",
    },
    FrameworkSignature {
        dependency: "webpack",
        framework_name: "webpack",
        category: "build-system",
    },
    FrameworkSignature {
        dependency: "jest",
        framework_name: "jest",
        category: "testing",
    },
    FrameworkSignature {
        dependency: "vitest",
        framework_name: "vitest",
        category: "testing",
    },
    FrameworkSignature {
        dependency: "playwright",
        framework_name: "playwright",
        category: "testing",
    },
    FrameworkSignature {
        dependency: "cypress",
        framework_name: "cypress",
        category: "testing",
    },
    FrameworkSignature {
        dependency: "tailwindcss",
        framework_name: "tailwindcss",
        category: "web-frontend",
    },
    FrameworkSignature {
        dependency: "electron",
        framework_name: "electron",
        category: "desktop",
    },
    // ── Rust ──────────────────────────────────────────────────────────
    FrameworkSignature {
        dependency: "actix-web",
        framework_name: "actix-web",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "axum",
        framework_name: "axum",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "rocket",
        framework_name: "rocket",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "warp",
        framework_name: "warp",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "tokio",
        framework_name: "tokio",
        category: "library",
    },
    FrameworkSignature {
        dependency: "clap",
        framework_name: "clap",
        category: "cli",
    },
    FrameworkSignature {
        dependency: "serde",
        framework_name: "serde",
        category: "library",
    },
    FrameworkSignature {
        dependency: "sqlx",
        framework_name: "sqlx",
        category: "data",
    },
    FrameworkSignature {
        dependency: "diesel",
        framework_name: "diesel",
        category: "data",
    },
    FrameworkSignature {
        dependency: "tonic",
        framework_name: "tonic",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "tauri",
        framework_name: "tauri",
        category: "desktop",
    },
    // ── Python ────────────────────────────────────────────────────────
    FrameworkSignature {
        dependency: "django",
        framework_name: "django",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "flask",
        framework_name: "flask",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "fastapi",
        framework_name: "fastapi",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "starlette",
        framework_name: "starlette",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "celery",
        framework_name: "celery",
        category: "library",
    },
    FrameworkSignature {
        dependency: "pytest",
        framework_name: "pytest",
        category: "testing",
    },
    FrameworkSignature {
        dependency: "pandas",
        framework_name: "pandas",
        category: "data",
    },
    FrameworkSignature {
        dependency: "numpy",
        framework_name: "numpy",
        category: "data",
    },
    FrameworkSignature {
        dependency: "tensorflow",
        framework_name: "tensorflow",
        category: "ml",
    },
    FrameworkSignature {
        dependency: "torch",
        framework_name: "pytorch",
        category: "ml",
    },
    FrameworkSignature {
        dependency: "scikit-learn",
        framework_name: "scikit-learn",
        category: "ml",
    },
    // ── Go ────────────────────────────────────────────────────────────
    FrameworkSignature {
        dependency: "github.com/gin-gonic/gin",
        framework_name: "gin",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "github.com/labstack/echo",
        framework_name: "echo",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "github.com/gofiber/fiber",
        framework_name: "fiber",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "github.com/go-chi/chi",
        framework_name: "chi",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "github.com/gorilla/mux",
        framework_name: "gorilla-mux",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "github.com/spf13/cobra",
        framework_name: "cobra",
        category: "cli",
    },
    FrameworkSignature {
        dependency: "github.com/spf13/viper",
        framework_name: "viper",
        category: "library",
    },
    // ── Ruby ──────────────────────────────────────────────────────────
    FrameworkSignature {
        dependency: "rails",
        framework_name: "rails",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "sinatra",
        framework_name: "sinatra",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "rspec",
        framework_name: "rspec",
        category: "testing",
    },
    FrameworkSignature {
        dependency: "hanami",
        framework_name: "hanami",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "sidekiq",
        framework_name: "sidekiq",
        category: "library",
    },
    // ── Java/Kotlin ──────────────────────────────────────────────────
    FrameworkSignature {
        dependency: "org.springframework.boot",
        framework_name: "spring-boot",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "io.quarkus",
        framework_name: "quarkus",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "io.ktor",
        framework_name: "ktor",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "io.micronaut",
        framework_name: "micronaut",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "io.vertx",
        framework_name: "vert.x",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "org.jetbrains.compose",
        framework_name: "compose-multiplatform",
        category: "mobile",
    },
    // ── .NET / C# ────────────────────────────────────────────────────────
    // SDK attribute extracted from <Project Sdk="..."> — most reliable aspnetcore signal
    FrameworkSignature {
        dependency: "Microsoft.NET.Sdk.Web",
        framework_name: "aspnetcore",
        category: "web-backend",
    },
    // Explicit framework reference / metapackage
    FrameworkSignature {
        dependency: "Microsoft.AspNetCore.App",
        framework_name: "aspnetcore",
        category: "web-backend",
    },
    // Common individual packages that imply aspnetcore
    FrameworkSignature {
        dependency: "Microsoft.AspNetCore.Mvc",
        framework_name: "aspnetcore",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "Microsoft.AspNetCore.Authentication.JwtBearer",
        framework_name: "aspnetcore",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "Swashbuckle.AspNetCore",
        framework_name: "aspnetcore",
        category: "web-backend",
    },
    // Entity Framework Core (separate framework)
    FrameworkSignature {
        dependency: "Microsoft.EntityFrameworkCore",
        framework_name: "entityframeworkcore",
        category: "data",
    },
    FrameworkSignature {
        dependency: "Microsoft.EntityFrameworkCore.SqlServer",
        framework_name: "entityframeworkcore",
        category: "data",
    },
    FrameworkSignature {
        dependency: "Microsoft.EntityFrameworkCore.InMemory",
        framework_name: "entityframeworkcore",
        category: "data",
    },
    // Avalonia UI
    FrameworkSignature {
        dependency: "Avalonia",
        framework_name: "avalonia",
        category: "desktop",
    },
    // .NET MAUI (detected via its main PackageReference)
    FrameworkSignature {
        dependency: "Microsoft.Maui.Controls",
        framework_name: "maui",
        category: "mobile",
    },
    FrameworkSignature {
        dependency: "Microsoft.Maui.Dependencies",
        framework_name: "maui",
        category: "mobile",
    },
    // ── C / C++ ──────────────────────────────────────────────────────────
    // CMake find_package() names (title-case) and vcpkg/Conan names (lowercase)

    // Testing
    FrameworkSignature {
        dependency: "GTest",
        framework_name: "googletest",
        category: "testing",
    },
    FrameworkSignature {
        dependency: "gtest",
        framework_name: "googletest",
        category: "testing",
    },
    FrameworkSignature {
        dependency: "googletest",
        framework_name: "googletest",
        category: "testing",
    },
    FrameworkSignature {
        dependency: "Catch2",
        framework_name: "catch2",
        category: "testing",
    },
    FrameworkSignature {
        dependency: "catch2",
        framework_name: "catch2",
        category: "testing",
    },
    FrameworkSignature {
        dependency: "doctest",
        framework_name: "doctest",
        category: "testing",
    },
    // GUI / Desktop
    FrameworkSignature {
        dependency: "Qt5",
        framework_name: "qt",
        category: "desktop",
    },
    FrameworkSignature {
        dependency: "Qt6",
        framework_name: "qt",
        category: "desktop",
    },
    FrameworkSignature {
        dependency: "qt5",
        framework_name: "qt",
        category: "desktop",
    },
    FrameworkSignature {
        dependency: "qt6",
        framework_name: "qt",
        category: "desktop",
    },
    FrameworkSignature {
        dependency: "SDL2",
        framework_name: "sdl2",
        category: "desktop",
    },
    FrameworkSignature {
        dependency: "sdl2",
        framework_name: "sdl2",
        category: "desktop",
    },
    FrameworkSignature {
        dependency: "SFML",
        framework_name: "sfml",
        category: "desktop",
    },
    FrameworkSignature {
        dependency: "sfml",
        framework_name: "sfml",
        category: "desktop",
    },
    // Web / networking
    FrameworkSignature {
        dependency: "cpp-httplib",
        framework_name: "cpp-httplib",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "Pistache",
        framework_name: "pistache",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "pistache",
        framework_name: "pistache",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "crow",
        framework_name: "crow",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "Drogon",
        framework_name: "drogon",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "drogon",
        framework_name: "drogon",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "oatpp",
        framework_name: "oatpp",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "Poco",
        framework_name: "poco",
        category: "web-backend",
    },
    FrameworkSignature {
        dependency: "poco",
        framework_name: "poco",
        category: "web-backend",
    },
    // Libraries
    FrameworkSignature {
        dependency: "Boost",
        framework_name: "boost",
        category: "library",
    },
    FrameworkSignature {
        dependency: "boost",
        framework_name: "boost",
        category: "library",
    },
    FrameworkSignature {
        dependency: "OpenSSL",
        framework_name: "openssl",
        category: "library",
    },
    FrameworkSignature {
        dependency: "openssl",
        framework_name: "openssl",
        category: "library",
    },
    // Data
    FrameworkSignature {
        dependency: "SQLite3",
        framework_name: "sqlite",
        category: "data",
    },
    FrameworkSignature {
        dependency: "sqlite3",
        framework_name: "sqlite",
        category: "data",
    },
    FrameworkSignature {
        dependency: "PostgreSQL",
        framework_name: "postgresql",
        category: "data",
    },
    FrameworkSignature {
        dependency: "libpq",
        framework_name: "postgresql",
        category: "data",
    },
];

/// Look up a dependency name in the registry.
///
/// For Go module paths, also tries prefix matching (e.g.
/// `github.com/gin-gonic/gin/v2` will match `github.com/gin-gonic/gin`).
pub fn lookup(dependency: &str) -> Option<&'static FrameworkSignature> {
    // Exact match first
    if let Some(sig) = FRAMEWORK_REGISTRY
        .iter()
        .find(|s| s.dependency == dependency)
    {
        return Some(sig);
    }

    // Prefix match for Go module paths (versioned imports like /v2, /v3).
    // Require the next character after the prefix to be '/' to avoid false
    // positives (e.g. "github.com/gorilla/muxer" should not match "github.com/gorilla/mux").
    if dependency.contains('/') {
        return FRAMEWORK_REGISTRY.iter().find(|s| {
            dependency.starts_with(s.dependency)
                && dependency.as_bytes().get(s.dependency.len()) == Some(&b'/')
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::types::FrameworkCategory;

    #[test]
    fn all_registry_entries_have_valid_categories() {
        let invalid: Vec<_> = FRAMEWORK_REGISTRY
            .iter()
            .filter(|sig| {
                matches!(
                    FrameworkCategory::from_str_insensitive(sig.category),
                    FrameworkCategory::Other(_)
                )
            })
            .map(|sig| sig.dependency)
            .collect();
        assert!(invalid.is_empty());
    }

    #[test]
    fn lookup_exact_match() {
        let sig = lookup("react").expect("react should be in registry");
        assert_eq!(sig.framework_name, "react");
        assert_eq!(sig.category, "web-frontend");
    }

    #[test]
    fn lookup_go_module_exact() {
        let sig = lookup("github.com/gin-gonic/gin").expect("gin should be in registry");
        assert_eq!(sig.framework_name, "gin");
    }

    #[test]
    fn lookup_go_module_versioned() {
        let sig = lookup("github.com/gin-gonic/gin/v2").expect("versioned gin should match");
        assert_eq!(sig.framework_name, "gin");
    }

    #[test]
    fn lookup_missing_returns_none() {
        assert!(lookup("nonexistent-package").is_none());
    }

    #[test]
    fn lookup_go_module_prefix_boundary() {
        // "github.com/gorilla/muxer" should NOT match "github.com/gorilla/mux"
        // because "muxer" is not a sub-path of "mux".
        assert!(lookup("github.com/gorilla/muxer").is_none());
        // But a genuine sub-path like /v2 should still match.
        let sig = lookup("github.com/gin-gonic/gin/v2").expect("versioned gin should match");
        assert_eq!(sig.framework_name, "gin");
    }

    #[test]
    fn test_lookup_aspnetcore_sdk_web() {
        let sig =
            lookup("Microsoft.NET.Sdk.Web").expect("Microsoft.NET.Sdk.Web should be in registry");
        assert_eq!(sig.framework_name, "aspnetcore");
        assert_eq!(sig.category, "web-backend");
    }

    #[test]
    fn test_lookup_aspnetcore_app() {
        let sig = lookup("Microsoft.AspNetCore.App")
            .expect("Microsoft.AspNetCore.App should be in registry");
        assert_eq!(sig.framework_name, "aspnetcore");
    }

    #[test]
    fn test_lookup_swashbuckle() {
        let sig =
            lookup("Swashbuckle.AspNetCore").expect("Swashbuckle.AspNetCore should be in registry");
        assert_eq!(sig.framework_name, "aspnetcore");
    }

    #[test]
    fn test_lookup_entityframeworkcore() {
        let sig = lookup("Microsoft.EntityFrameworkCore")
            .expect("Microsoft.EntityFrameworkCore should be in registry");
        assert_eq!(sig.framework_name, "entityframeworkcore");
        assert_eq!(sig.category, "data");
    }

    #[test]
    fn test_lookup_compose_multiplatform() {
        let sig =
            lookup("org.jetbrains.compose").expect("org.jetbrains.compose should be in registry");
        assert_eq!(sig.framework_name, "compose-multiplatform");
        assert_eq!(sig.category, "mobile");
    }

    #[test]
    fn test_lookup_vertx() {
        let sig = lookup("io.vertx").expect("io.vertx should be in registry");
        assert_eq!(sig.framework_name, "vert.x");
        assert_eq!(sig.category, "web-backend");
    }

    #[test]
    fn test_lookup_hanami() {
        let sig = lookup("hanami").expect("hanami should be in registry");
        assert_eq!(sig.framework_name, "hanami");
        assert_eq!(sig.category, "web-backend");
    }

    #[test]
    fn registry_has_entries_for_all_ecosystems() {
        // Spot-check at least one entry per ecosystem
        let checks = [
            "react",
            "actix-web",
            "django",
            "github.com/gin-gonic/gin",
            "rails",
            "hanami",
            "org.springframework.boot",
            "Microsoft.NET.Sdk.Web",
            "GTest",
            "Qt6",
        ];
        for dep in checks {
            assert!(lookup(dep).is_some());
        }
    }

    #[test]
    fn test_lookup_gtest_cmake_name() {
        let sig = lookup("GTest").expect("GTest should be in registry");
        assert_eq!(sig.framework_name, "googletest");
        assert_eq!(sig.category, "testing");
    }

    #[test]
    fn test_lookup_gtest_pkg_name() {
        let sig = lookup("gtest").expect("gtest should be in registry");
        assert_eq!(sig.framework_name, "googletest");
    }

    #[test]
    fn test_lookup_qt6() {
        let sig = lookup("Qt6").expect("Qt6 should be in registry");
        assert_eq!(sig.framework_name, "qt");
        assert_eq!(sig.category, "desktop");
    }

    #[test]
    fn test_lookup_boost() {
        let sig = lookup("Boost").expect("Boost should be in registry");
        assert_eq!(sig.framework_name, "boost");
        assert_eq!(sig.category, "library");
    }

    #[test]
    fn test_lookup_sqlite3() {
        let sig = lookup("SQLite3").expect("SQLite3 should be in registry");
        assert_eq!(sig.framework_name, "sqlite");
        assert_eq!(sig.category, "data");
    }

    #[test]
    fn test_lookup_avalonia() {
        let sig = lookup("Avalonia").expect("Avalonia should be in registry");
        assert_eq!(sig.framework_name, "avalonia");
        assert_eq!(sig.category, "desktop");
    }

    #[test]
    fn test_lookup_maui_controls() {
        let sig = lookup("Microsoft.Maui.Controls")
            .expect("Microsoft.Maui.Controls should be in registry");
        assert_eq!(sig.framework_name, "maui");
        assert_eq!(sig.category, "mobile");
    }

    #[test]
    fn test_lookup_maui_dependencies() {
        let sig = lookup("Microsoft.Maui.Dependencies")
            .expect("Microsoft.Maui.Dependencies should be in registry");
        assert_eq!(sig.framework_name, "maui");
        assert_eq!(sig.category, "mobile");
    }

    #[test]
    fn test_lookup_drogon_cmake_name() {
        let sig = lookup("Drogon").expect("Drogon should be in registry");
        assert_eq!(sig.framework_name, "drogon");
        assert_eq!(sig.category, "web-backend");
    }

    #[test]
    fn test_lookup_drogon_pkg_name() {
        let sig = lookup("drogon").expect("drogon should be in registry");
        assert_eq!(sig.framework_name, "drogon");
        assert_eq!(sig.category, "web-backend");
    }

    #[test]
    fn test_lookup_oatpp() {
        let sig = lookup("oatpp").expect("oatpp should be in registry");
        assert_eq!(sig.framework_name, "oatpp");
        assert_eq!(sig.category, "web-backend");
    }

    #[test]
    fn test_lookup_poco_cmake_name() {
        let sig = lookup("Poco").expect("Poco should be in registry");
        assert_eq!(sig.framework_name, "poco");
        assert_eq!(sig.category, "web-backend");
    }

    #[test]
    fn test_lookup_poco_pkg_name() {
        let sig = lookup("poco").expect("poco should be in registry");
        assert_eq!(sig.framework_name, "poco");
        assert_eq!(sig.category, "web-backend");
    }
}

output "metrics_exporter_role_arn" {
  description = "ARN of the IAM role to annotate on the metrics-exporter Kubernetes service account."
  value       = aws_iam_role.metrics_exporter.arn
}

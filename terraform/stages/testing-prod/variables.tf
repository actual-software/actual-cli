variable "aws_region" {
  description = "AWS region where resources are deployed."
  type        = string
  default     = "us-east-1"
}

variable "cluster_oidc_provider_arn" {
  description = "ARN of the OIDC provider for the testing-prod EKS cluster (e.g. arn:aws:iam::123456789012:oidc-provider/oidc.eks.us-east-1.amazonaws.com/id/EXAMPLED539D4633E53DE1B71EXAMPLE)."
  type        = string
}

variable "cluster_oidc_provider_url" {
  description = "Hostname of the OIDC provider without the https:// scheme (e.g. oidc.eks.us-east-1.amazonaws.com/id/EXAMPLED539D4633E53DE1B71EXAMPLE)."
  type        = string
}

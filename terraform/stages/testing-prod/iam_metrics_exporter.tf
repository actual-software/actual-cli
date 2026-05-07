locals {
  metrics_exporter_service_account = "system:serviceaccount:monitoring:metrics-exporter"
}

data "aws_iam_policy_document" "metrics_exporter_assume_role" {
  statement {
    effect  = "Allow"
    actions = ["sts:AssumeRoleWithWebIdentity"]

    principals {
      type        = "Federated"
      identifiers = [var.cluster_oidc_provider_arn]
    }

    condition {
      test     = "StringEquals"
      variable = "${var.cluster_oidc_provider_url}:sub"
      values   = [local.metrics_exporter_service_account]
    }

    condition {
      test     = "StringEquals"
      variable = "${var.cluster_oidc_provider_url}:aud"
      values   = ["sts.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "metrics_exporter" {
  name               = "testing-prod-metrics-exporter"
  description        = "IRSA role for the metrics-exporter service account in the monitoring namespace of the testing-prod cluster."
  assume_role_policy = data.aws_iam_policy_document.metrics_exporter_assume_role.json
}

data "aws_iam_policy_document" "metrics_exporter_cloudwatch" {
  statement {
    effect = "Allow"
    actions = [
      "cloudwatch:GetMetricData",
      "cloudwatch:ListMetrics",
      "logs:DescribeLogGroups",
      "logs:GetLogEvents",
    ]
    resources = ["*"]
  }
}

resource "aws_iam_policy" "metrics_exporter_cloudwatch" {
  name        = "testing-prod-metrics-exporter-cloudwatch"
  description = "Read-only CloudWatch and CloudWatch Logs access for the metrics-exporter workload."
  policy      = data.aws_iam_policy_document.metrics_exporter_cloudwatch.json
}

resource "aws_iam_role_policy_attachment" "metrics_exporter_cloudwatch" {
  role       = aws_iam_role.metrics_exporter.name
  policy_arn = aws_iam_policy.metrics_exporter_cloudwatch.arn
}

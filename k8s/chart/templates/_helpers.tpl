{{- define "tx3-fluent.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "tx3-fluent.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $name := default .Chart.Name .Values.nameOverride -}}
{{- if contains $name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "tx3-fluent.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "tx3-fluent.labels" -}}
helm.sh/chart: {{ include "tx3-fluent.chart" . }}
{{ include "tx3-fluent.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{- define "tx3-fluent.selectorLabels" -}}
app.kubernetes.io/name: {{ include "tx3-fluent.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{/*
A release tag or an exact build: `latest` would let a restart change the
version.
*/}}
{{- define "tx3-fluent.image" -}}
{{- $img := .Values.image -}}
{{- $repository := required "image.repository is required" $img.repository -}}
{{- $tag := required "image.tag is required" $img.tag -}}
{{- if eq $tag "latest" -}}
{{- fail "image.tag must be a release tag or sha-<commit>, never latest" -}}
{{- end -}}
{{- if $img.digest -}}
{{- printf "%s:%s@%s" $repository $tag $img.digest -}}
{{- else -}}
{{- printf "%s:%s" $repository $tag -}}
{{- end -}}
{{- end -}}

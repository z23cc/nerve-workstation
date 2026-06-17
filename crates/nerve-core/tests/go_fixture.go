package main

import (
	"fmt"
	"strings"
)

type Greeter interface {
	Greet(name string) string
}

type Service struct {
	prefix string
}

const MaxRetries = 3

var defaultName = "world"

func (s *Service) Greet(name string) string {
	return s.prefix + strings.ToUpper(name)
}

func NewService() *Service {
	return &Service{prefix: fmt.Sprintf("[%d] ", MaxRetries)}
}

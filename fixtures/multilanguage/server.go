package fixture

type Server struct{}

func (s *Server) Serve() {
	prepare()
}

func prepare() {}

